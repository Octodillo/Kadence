//! The audio-thread half of a plugin whose main-thread calls may overlap `process`.
//!
//! A format that can [`split`](PluginInstance::split) keeps what `process` works on in a
//! [`SharedAudio`]. The audio thread plays it through the [`SharedProcessor`] that
//! [`SharedAudio::lend`] hands out; the main thread reaches it with [`SharedAudio::main`],
//! only for the few short calls that must not overlap a block. A block that comes during one
//! of those waits a few microseconds, then is silent, and what it carried is kept in
//! [`LateEvents`] for the next block: a note-off or a reset is late, never lost.
//!
//! [`PluginInstance::split`]: crate::plugin::PluginInstance::split

use std::any::Any;
use std::sync::Arc;

use crate::event::{Event, EventKind, EventList};
use crate::exclusive::Exclusive;
use crate::node::ProcessContext;
use crate::plugin::PluginProcessor;

/// How many events of skipped blocks a [`SharedProcessor`] keeps. More are dropped.
pub const LATE_EVENT_CAPACITY: usize = 1024;

/// What a plugin missed in blocks it could not be run for: their events and a reset, to
/// deliver at the start of the next block it runs. Real-time safe once made.
#[derive(Debug)]
pub struct LateEvents {
    events: EventList,
    reset: bool,
}

impl LateEvents {
    /// Allocates room for `capacity` late events.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: EventList::with_capacity(capacity),
            reset: false,
        }
    }

    /// Notes a reset for the next block that runs. Notes still waiting are older than the
    /// reset, which would have ended them, so only the parameter changes stay.
    pub fn reset(&mut self) {
        self.reset = true;
        self.events
            .retain(|e| matches!(e.kind, EventKind::ParamValue { .. }));
    }

    /// Whether a reset is owed, once: the caller resets the plugin before it runs the block.
    pub fn take_reset(&mut self) -> bool {
        std::mem::take(&mut self.reset)
    }

    /// Runs `f` on a block the plugin was reached for, with the late events at frame 0 ahead
    /// of the block's own.
    pub fn deliver<R>(
        &mut self,
        ctx: &mut ProcessContext,
        f: impl FnOnce(&mut ProcessContext) -> R,
    ) -> R {
        if self.events.is_empty() {
            return f(ctx);
        }
        for e in ctx.events_in {
            // A full list drops the newest events, as a full block does.
            let _ = self.events.push(*e);
        }
        let r = ctx.with_events_in(&self.events, f);
        self.events.clear();
        r
    }

    /// A block the plugin could not be reached for: silence, and its events wait.
    pub fn skip(&mut self, ctx: &mut ProcessContext) {
        ctx.silence_outputs();
        for e in ctx.events_in {
            let _ = self.events.push(Event { frame: 0, ..*e });
        }
    }

    /// Whether events or a reset wait for the next block.
    pub fn is_pending(&self) -> bool {
        self.reset || !self.events.is_empty()
    }
}

/// What a split plugin's `process` works on. Both calls run on the audio thread, or on the
/// main thread while the audio thread is kept out.
pub trait SplitAudio: Send + 'static {
    fn process(&mut self, ctx: &mut ProcessContext);
    fn reset(&mut self);
}

/// The main-thread side of a split plugin's audio state `A`: owns it together with the
/// [`SharedProcessor`] it lends to the audio thread.
pub struct SharedAudio<A> {
    audio: Arc<Exclusive<A>>,
    lent: bool,
}

impl<A: SplitAudio> SharedAudio<A> {
    pub fn new(audio: A) -> Self {
        Self {
            audio: Arc::new(Exclusive::new(audio)),
            lent: false,
        }
    }

    /// The state itself, for a caller that needs it next to a `&mut` of the object that holds
    /// this: clone it, then use [`Exclusive::main`].
    pub fn exclusive(&self) -> &Arc<Exclusive<A>> {
        &self.audio
    }

    /// Runs `f` on the audio state, on the main thread. A block that comes meanwhile is
    /// skipped once it has waited a few microseconds, so keep `f` short.
    pub fn main<R>(&self, f: impl FnOnce(&mut A) -> R) -> R {
        self.audio.main(f)
    }

    /// Whether the processor half is out.
    pub fn is_lent(&self) -> bool {
        self.lent
    }

    /// Hands out the audio-thread half, for [`PluginInstance::split`]; `None` while one is
    /// out. Allocates.
    ///
    /// [`PluginInstance::split`]: crate::plugin::PluginInstance::split
    pub fn lend(&mut self) -> Option<Box<dyn PluginProcessor>> {
        if self.lent {
            return None;
        }
        self.lent = true;
        Some(Box::new(SharedProcessor {
            audio: Arc::clone(&self.audio),
            late: LateEvents::with_capacity(LATE_EVENT_CAPACITY),
        }))
    }

    /// Takes back the half [`lend`](Self::lend) handed out, for [`PluginInstance::join`].
    /// Returns false, and changes nothing, for a processor that is not this one's. What the
    /// half still held for a later block is dropped: the host joins to prepare the plugin
    /// again or to drop it.
    ///
    /// [`PluginInstance::join`]: crate::plugin::PluginInstance::join
    pub fn take_back(&mut self, processor: Box<dyn PluginProcessor>) -> bool {
        match processor.into_any().downcast::<SharedProcessor<A>>() {
            Ok(half) if Arc::ptr_eq(&half.audio, &self.audio) => {
                self.lent = false;
                true
            }
            _ => false,
        }
    }

    /// The owner's own [`Node::process`](crate::node::Node::process): plays the block unless
    /// the half is out or the main thread holds the state, and is silent then.
    pub fn process(&mut self, ctx: &mut ProcessContext) {
        if self.lent || self.audio.try_audio(|a| a.process(ctx)).is_none() {
            ctx.silence_outputs();
        }
    }

    /// The owner's own [`Node::reset`](crate::node::Node::reset); does nothing while the half
    /// is out.
    pub fn reset(&mut self) {
        if !self.lent {
            self.audio.try_audio(A::reset);
        }
    }
}

/// The audio-thread half of a split plugin, from [`SharedAudio::lend`].
///
/// A block that comes while the main thread holds the audio state waits a few microseconds
/// for it, then is silent; its events and any reset reach the plugin with the next block.
/// Give it back with [`SharedAudio::take_back`] before the owner is dropped, or the state's
/// last reference goes wherever this is dropped.
pub struct SharedProcessor<A> {
    audio: Arc<Exclusive<A>>,
    late: LateEvents,
}

impl<A: SplitAudio> PluginProcessor for SharedProcessor<A> {
    fn process(&mut self, ctx: &mut ProcessContext) {
        let late = &mut self.late;
        let ran = self.audio.try_audio_spin(|a| {
            if late.take_reset() {
                a.reset();
            }
            late.deliver(ctx, |ctx| a.process(ctx));
        });
        if ran.is_none() {
            late.skip(ctx);
        }
    }

    /// Takes effect at the start of the next block the plugin runs.
    fn reset(&mut self) {
        self.late.reset();
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        self
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::node::TransportState;

    /// Records what reaches it and plays a constant.
    #[derive(Default)]
    struct Probe {
        seen: Vec<Seen>,
    }

    #[derive(Debug, PartialEq)]
    enum Seen {
        Reset,
        Block(Vec<Event>),
    }

    impl SplitAudio for Probe {
        fn process(&mut self, ctx: &mut ProcessContext) {
            self.seen
                .push(Seen::Block(ctx.events_in.as_slice().to_vec()));
            for out in ctx.audio_out.iter_mut() {
                out.fill(1.0);
            }
        }
        fn reset(&mut self) {
            self.seen.push(Seen::Reset);
        }
    }

    fn on(frame: u32, key: u8) -> Event {
        Event {
            frame,
            kind: EventKind::NoteOn {
                channel: 0,
                key,
                velocity: 1.0,
                note_id: -1,
            },
        }
    }

    fn off(frame: u32, key: u8) -> Event {
        Event {
            frame,
            kind: EventKind::NoteOff {
                channel: 0,
                key: Some(key),
                velocity: 0.0,
                note_id: -1,
            },
        }
    }

    fn param(frame: u32, value: f64) -> Event {
        Event {
            frame,
            kind: EventKind::ParamValue { param: 7, value },
        }
    }

    /// Runs one block of `events` and returns the first output sample.
    fn block(half: &mut dyn PluginProcessor, events: &[Event]) -> f32 {
        let mut events_in = EventList::with_capacity(8);
        for e in events {
            events_in.push(*e).unwrap();
        }
        let mut events_out = EventList::with_capacity(0);
        let mut out = [0.5f32; 4];
        let mut audio_out = [&mut out[..]];
        half.process(&mut ProcessContext {
            frames: 4,
            transport: &TransportState::default(),
            audio_in: &[],
            audio_out: &mut audio_out,
            events_in: &events_in,
            events_out: &mut events_out,
        });
        out[0]
    }

    fn seen(shared: &SharedAudio<Probe>) -> Vec<Seen> {
        shared.main(|p| std::mem::take(&mut p.seen))
    }

    #[test]
    fn a_skipped_block_is_silent_and_its_events_come_with_the_next() {
        let mut shared = SharedAudio::new(Probe::default());
        let mut half = shared.lend().unwrap();
        assert!(shared.lend().is_none(), "one half at a time");

        assert_eq!(block(half.as_mut(), &[on(1, 60)]), 1.0);
        let skipped = shared.main(|_| block(half.as_mut(), &[off(2, 60), param(3, 0.5)]));
        assert_eq!(skipped, 0.0);
        assert_eq!(block(half.as_mut(), &[on(0, 62), on(3, 64)]), 1.0);
        assert_eq!(block(half.as_mut(), &[]), 1.0);
        assert_eq!(
            seen(&shared),
            [
                Seen::Block(vec![on(1, 60)]),
                Seen::Block(vec![off(0, 60), param(0, 0.5), on(0, 62), on(3, 64)]),
                Seen::Block(vec![]),
            ]
        );
    }

    #[test]
    fn a_reset_during_a_held_state_comes_before_the_next_block() {
        let mut shared = SharedAudio::new(Probe::default());
        let mut half = shared.lend().unwrap();
        shared.main(|_| {
            block(half.as_mut(), &[on(1, 60), param(2, 0.25)]);
            half.reset();
            block(half.as_mut(), &[off(3, 61)]);
        });
        block(half.as_mut(), &[]);
        // The note from before the reset is gone; the parameter change and what came after
        // the reset are not.
        assert_eq!(
            seen(&shared),
            [Seen::Reset, Seen::Block(vec![param(0, 0.25), off(0, 61)])]
        );
        half.reset();
        block(half.as_mut(), &[]);
        assert_eq!(seen(&shared), [Seen::Reset, Seen::Block(vec![])]);
    }

    #[test]
    fn the_owner_is_silent_while_its_half_is_out() {
        let mut shared = SharedAudio::new(Probe::default());
        let mut other = SharedAudio::new(Probe::default());
        let half = shared.lend().unwrap();
        assert!(!other.take_back(half), "not its own half");
        assert!(shared.is_lent());

        let mut events_out = EventList::with_capacity(0);
        let mut play = |shared: &mut SharedAudio<Probe>| {
            let mut out = [0.5f32; 4];
            let mut audio_out = [&mut out[..]];
            shared.process(&mut ProcessContext {
                frames: 4,
                transport: &TransportState::default(),
                audio_in: &[],
                audio_out: &mut audio_out,
                events_in: &EventList::with_capacity(0),
                events_out: &mut events_out,
            });
            out[0]
        };
        shared.reset();
        assert_eq!(play(&mut shared), 0.0);
        assert_eq!(seen(&shared), []);

        let half = shared.lend();
        assert!(half.is_none());
        let mut fresh = SharedAudio::new(Probe::default());
        let half = fresh.lend().unwrap();
        assert!(fresh.take_back(half));
        fresh.reset();
        assert_eq!(play(&mut fresh), 1.0);
        assert_eq!(seen(&fresh), [Seen::Reset, Seen::Block(vec![])]);
    }

    #[test]
    fn late_events_past_the_capacity_are_dropped_not_grown() {
        let mut late = LateEvents::with_capacity(2);
        let mut events_in = EventList::with_capacity(3);
        for k in 0..3 {
            events_in.push(off(k, 60 + k as u8)).unwrap();
        }
        let mut events_out = EventList::with_capacity(0);
        let mut ctx = ProcessContext {
            frames: 4,
            transport: &TransportState::default(),
            audio_in: &[],
            audio_out: &mut [],
            events_in: &events_in,
            events_out: &mut events_out,
        };
        late.skip(&mut ctx);
        assert!(late.is_pending());
        let got = late.deliver(&mut ctx, |ctx| ctx.events_in.as_slice().to_vec());
        assert_eq!(got, [off(0, 60), off(0, 61)]);
        assert!(!late.is_pending());
    }
}
