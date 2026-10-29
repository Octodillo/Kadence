//! Mixer logic that spans several strips.

/// Mute and solo buttons of one strip.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SoloState {
    pub muted: bool,
    pub soloed: bool,
    /// Keeps playing when other strips are soloed (typical for returns).
    pub solo_safe: bool,
}

/// Which strips should be silent, given each strip's buttons and the routing
/// between strips (`(from, to)` index pairs: outputs and sends).
///
/// A muted strip is always silent. While any strip is soloed, a strip plays
/// only if it is soloed, solo safe, feeds a soloed strip (directly or
/// through other strips), or is fed by one. So soloing a bus keeps the
/// tracks routed into it, and soloing a track keeps the buses it sends to.
pub fn effective_mutes(strips: &[SoloState], feeds: &[(usize, usize)]) -> Vec<bool> {
    let n = strips.len();
    if !strips.iter().any(|s| s.soloed) {
        return strips.iter().map(|s| s.muted).collect();
    }
    let mut audible: Vec<bool> = strips.iter().map(|s| s.soloed || s.solo_safe).collect();
    let soloed: Vec<usize> = (0..n).filter(|&i| strips[i].soloed).collect();
    let valid = |&&(a, b): &&(usize, usize)| a < n && b < n;
    // Downstream of a soloed strip, then upstream of one.
    for downstream in [true, false] {
        let mut seen = vec![false; n];
        let mut stack = soloed.clone();
        while let Some(i) = stack.pop() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            audible[i] = true;
            for &(a, b) in feeds.iter().filter(valid) {
                let (from, to) = if downstream { (a, b) } else { (b, a) };
                if from == i && !seen[to] {
                    stack.push(to);
                }
            }
        }
    }
    strips
        .iter()
        .zip(audible)
        .map(|(s, a)| s.muted || !a)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OFF: SoloState = SoloState {
        muted: false,
        soloed: false,
        solo_safe: false,
    };
    const SOLO: SoloState = SoloState {
        soloed: true,
        ..OFF
    };
    const MUTE: SoloState = SoloState { muted: true, ..OFF };

    #[test]
    fn no_solo_means_mute_buttons_only() {
        assert_eq!(effective_mutes(&[OFF, MUTE], &[]), vec![false, true]);
    }

    #[test]
    fn solo_keeps_its_signal_path() {
        // 0, 1: tracks; 2: reverb bus fed by both; 3: group bus fed by 1.
        let feeds = [(0, 2), (1, 2), (1, 3)];
        assert_eq!(
            effective_mutes(&[SOLO, OFF, OFF, OFF], &feeds),
            vec![false, true, false, true]
        );
        // Soloing the group keeps its source track but not the other one.
        assert_eq!(
            effective_mutes(&[OFF, OFF, OFF, SOLO], &feeds),
            vec![true, false, true, false]
        );
    }

    #[test]
    fn mute_beats_solo_and_safe_strips_play() {
        let safe = SoloState {
            solo_safe: true,
            ..OFF
        };
        let both = SoloState {
            muted: true,
            soloed: true,
            ..OFF
        };
        assert_eq!(
            effective_mutes(&[both, OFF, safe], &[]),
            vec![true, true, false]
        );
    }
}
