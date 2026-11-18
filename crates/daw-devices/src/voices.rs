//! A fixed pool of instrument voices with stealing.
//!
//! An instrument plays at most `limit` notes at once. A note beyond that
//! steals a voice: a released one if there is any (the quietest), otherwise
//! the oldest. The stolen voice is not cut off; it fades out over a few
//! milliseconds in one of the pool's spare slots while its replacement
//! starts. [`Note::SoundOff`](crate::events::Note) fades every voice the same
//! way.
//!
//! The pool owns which slot is free, playing or fading, each voice's age and
//! the fade; an instrument supplies what a voice is ([`Stealable`]) and renders
//! it. Nothing here allocates.

/// Fade-out time for a stolen voice.
const STEAL_FADE_MS: f64 = 3.0;

/// Slots to give a pool beyond the most voices that may play at once, so that
/// stolen voices can fade out while their replacements start.
pub(crate) const SPARE_VOICES: usize = 8;

/// What the pool needs to know about a voice to choose which one to steal.
pub(crate) trait Stealable: Copy + Default {
    /// Whether its note was released and it is dying away by itself.
    fn is_releasing(&self) -> bool;
    /// How loud it is now. Only compared between releasing voices.
    fn level(&self) -> f32;
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum State {
    #[default]
    Free,
    /// Counts towards the limit.
    Playing,
    /// Stolen, with this much of its fade left (1 down to 0).
    Fading(f32),
}

#[derive(Clone, Copy, Default)]
struct Slot<V> {
    state: State,
    /// The pool's clock when the note started; lower is older.
    age: u64,
    voice: V,
}

/// `N` slots: the most voices that may play at once plus spares for fades.
pub(crate) struct VoicePool<V, const N: usize> {
    slots: [Slot<V>; N],
    clock: u64,
    fade_step: f32,
}

impl<V: Stealable, const N: usize> VoicePool<V, N> {
    pub fn new(sample_rate: f64) -> Self {
        let mut pool = Self {
            slots: [Slot::default(); N],
            clock: 0,
            fade_step: 0.0,
        };
        pool.set_sample_rate(sample_rate);
        pool
    }

    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        self.fade_step = (1000.0 / (STEAL_FADE_MS * sample_rate)) as f32;
    }

    /// Voices playing, not counting stolen ones fading out.
    pub fn playing(&self) -> usize {
        self.iter_playing().count()
    }

    pub fn iter_playing(&self) -> impl Iterator<Item = &V> {
        self.slots
            .iter()
            .filter(|s| s.state == State::Playing)
            .map(|s| &s.voice)
    }

    pub fn iter_playing_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.slots
            .iter_mut()
            .filter(|s| s.state == State::Playing)
            .map(|s| &mut s.voice)
    }

    /// Stolen voices that are still fading out.
    #[cfg(test)]
    pub fn iter_fading(&self) -> impl Iterator<Item = &V> {
        self.slots
            .iter()
            .filter(|s| matches!(s.state, State::Fading(_)))
            .map(|s| &s.voice)
    }

    /// Every slot's voice, free ones included: for settings all voices share.
    pub fn iter_all_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.slots.iter_mut().map(|s| &mut s.voice)
    }

    /// A voice for a new note, as `V::default()`, stealing while `limit`
    /// voices or more are playing (more, after the limit was lowered). A
    /// limit outside 1 to `N` counts as the nearest of those; one below `N`
    /// leaves slots for fades.
    pub fn start(&mut self, limit: usize) -> &mut V {
        self.start_or_reuse(limit, |_| false).0
    }

    /// As [`start`](Self::start), but a playing voice that `reuse` accepts is
    /// handed back as it is, so a retriggered note continues from where its
    /// voice was. The flag says whether that happened.
    pub fn start_or_reuse(&mut self, limit: usize, reuse: impl Fn(&V) -> bool) -> (&mut V, bool) {
        let reused = self
            .slots
            .iter()
            .position(|s| s.state == State::Playing && reuse(&s.voice));
        let limit = limit.clamp(1, N);
        let index = reused.unwrap_or_else(|| {
            while self.playing() >= limit {
                self.steal();
            }
            let index = self.free_slot();
            self.slots[index].voice = V::default();
            index
        });
        self.clock += 1;
        let slot = &mut self.slots[index];
        slot.state = State::Playing;
        slot.age = self.clock;
        (&mut slot.voice, reused.is_some())
    }

    /// Start fading out the quietest releasing voice, else the oldest one.
    fn steal(&mut self) {
        let victim = self
            .slots
            .iter_mut()
            .filter(|s| s.state == State::Playing)
            .min_by(|a, b| {
                let (ra, rb) = (a.voice.is_releasing(), b.voice.is_releasing());
                rb.cmp(&ra).then_with(|| {
                    if ra {
                        a.voice.level().total_cmp(&b.voice.level())
                    } else {
                        a.age.cmp(&b.age)
                    }
                })
            });
        if let Some(slot) = victim {
            slot.state = State::Fading(1.0);
        }
    }

    /// A free slot, or when every spare is still fading, the one whose fade
    /// is nearest its end, cut short.
    fn free_slot(&self) -> usize {
        if let Some(index) = self.slots.iter().position(|s| s.state == State::Free) {
            return index;
        }
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, s)| match s.state {
                State::Fading(left) => Some((index, left)),
                _ => None,
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
            .expect("a voice was stolen if none was free, so one is fading")
    }

    /// Fade out everything that plays.
    pub fn sound_off(&mut self) {
        for slot in &mut self.slots {
            if slot.state == State::Playing {
                slot.state = State::Fading(1.0);
            }
        }
    }

    /// Stop everything at once.
    pub fn clear(&mut self) {
        self.slots = [Slot::default(); N];
    }

    /// One sample of every sounding voice. `render` gets the voice and its
    /// fade gain (1 unless stolen) and says whether the voice goes on; a
    /// voice that ended, or whose fade did, frees its slot.
    pub fn render(&mut self, mut render: impl FnMut(&mut V, f32) -> bool) {
        for slot in &mut self.slots {
            let fade = match slot.state {
                State::Free => continue,
                State::Playing => 1.0,
                State::Fading(left) => {
                    let left = left - self.fade_step;
                    slot.state = State::Fading(left);
                    left
                }
            };
            if fade <= 0.0 || !render(&mut slot.voice, fade) {
                slot.state = State::Free;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Default, PartialEq, Debug)]
    struct Tone {
        key: u8,
        releasing: bool,
        level: f32,
    }

    impl Stealable for Tone {
        fn is_releasing(&self) -> bool {
            self.releasing
        }
        fn level(&self) -> f32 {
            self.level
        }
    }

    type Pool = VoicePool<Tone, 6>;
    const LIMIT: usize = 4;

    fn play(pool: &mut Pool, key: u8) {
        *pool.start(LIMIT) = Tone {
            key,
            releasing: false,
            level: 1.0,
        };
    }

    fn keys<'a>(voices: impl Iterator<Item = &'a Tone>) -> Vec<u8> {
        let mut keys: Vec<u8> = voices.map(|v| v.key).collect();
        keys.sort_unstable();
        keys
    }

    fn full() -> Pool {
        let mut pool = Pool::new(48_000.0);
        for key in 0..LIMIT as u8 {
            play(&mut pool, key);
        }
        pool
    }

    #[test]
    fn the_oldest_voice_is_stolen_and_fades() {
        let mut pool = full();
        play(&mut pool, 10);
        assert_eq!(keys(pool.iter_playing()), [1, 2, 3, 10]);
        assert_eq!(keys(pool.iter_fading()), [0]);
    }

    #[test]
    fn the_quietest_releasing_voice_is_stolen_before_any_held_one() {
        let mut pool = full();
        for v in pool.iter_playing_mut().filter(|v| v.key >= 2) {
            v.releasing = true;
            v.level = if v.key == 3 { 0.1 } else { 0.5 };
        }
        play(&mut pool, 10);
        assert_eq!(keys(pool.iter_fading()), [3]);
        play(&mut pool, 11);
        assert_eq!(keys(pool.iter_fading()), [2, 3]);
    }

    #[test]
    fn a_reused_voice_keeps_its_state_and_becomes_the_newest() {
        let mut pool = full();
        let (voice, reused) = pool.start_or_reuse(LIMIT, |v| v.key == 0);
        assert!(reused);
        assert_eq!(voice.level, 1.0);
        assert_eq!(pool.playing(), LIMIT);
        // Key 0 was the oldest; now key 1 is.
        play(&mut pool, 10);
        assert_eq!(keys(pool.iter_fading()), [1]);
    }

    #[test]
    fn a_new_voice_starts_from_default_whatever_its_slot_held() {
        // In a slot whose voice ended by itself.
        let mut pool = full();
        pool.render(|v, _| v.key != 2);
        assert_eq!(*pool.start(LIMIT), Tone::default());

        // In a slot whose fade ran out.
        let mut pool = full();
        pool.sound_off();
        for _ in 0..150 {
            pool.render(|_, _| true);
        }
        for _ in 0..LIMIT {
            assert_eq!(*pool.start(LIMIT), Tone::default());
        }

        // In a slot whose fade was cut short: with every slot playing or
        // fading, the next voice takes a fading one.
        let mut pool = full();
        for key in 10..12 {
            play(&mut pool, key);
        }
        assert_eq!(pool.iter_fading().count(), 2);
        assert_eq!(*pool.start(LIMIT), Tone::default());
    }

    #[test]
    fn a_limit_above_the_pool_size_counts_as_the_pool_size() {
        let mut pool = Pool::new(48_000.0);
        for key in 0..8 {
            *pool.start(usize::MAX) = Tone {
                key,
                releasing: false,
                level: 1.0,
            };
        }
        assert_eq!(keys(pool.iter_playing()), [2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn a_burst_cuts_the_fade_nearest_its_end_and_never_exceeds_the_limit() {
        let mut pool = full();
        play(&mut pool, 10);
        // Key 0 has faded for a while when the others are stolen.
        for _ in 0..10 {
            pool.render(|_, _| true);
        }
        play(&mut pool, 11);
        assert_eq!(keys(pool.iter_fading()), [0, 1]);
        // No slot is free: key 0's fade is cut for the new voice.
        play(&mut pool, 12);
        assert_eq!(keys(pool.iter_fading()), [1, 2]);
        assert_eq!(pool.playing(), LIMIT);
    }

    #[test]
    fn a_lowered_limit_steals_down_to_it() {
        let mut pool = full();
        pool.start(2).key = 10;
        assert_eq!(keys(pool.iter_playing()), [3, 10]);
    }

    #[test]
    fn a_stolen_voice_fades_to_nothing_then_frees_its_slot() {
        let mut pool = full();
        pool.sound_off();
        assert_eq!(pool.playing(), 0);
        let mut gains = Vec::new();
        // 3 ms at 48 kHz is 144 samples.
        for _ in 0..150 {
            pool.render(|v, fade| {
                if v.key == 0 {
                    gains.push(fade);
                }
                true
            });
        }
        assert!(gains.len() >= 142 && gains.len() <= 144, "{}", gains.len());
        assert!(gains.windows(2).all(|w| w[1] < w[0]));
        assert!(gains[0] < 1.0 && *gains.last().unwrap() > 0.0);
        assert_eq!(pool.iter_fading().count(), 0);
    }

    #[test]
    fn a_voice_that_says_it_ended_frees_its_slot() {
        let mut pool = full();
        pool.render(|v, _| v.key != 2);
        assert_eq!(keys(pool.iter_playing()), [0, 1, 3]);
    }
}
