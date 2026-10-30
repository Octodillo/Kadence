//! Properties every grid must keep, for random meters and grids.

use daw_core::time::TickTime;
use daw_midi::grid::{Division, Feel, Grid, LineKind, MAX_TICK, Meter, MeterPoint};
use proptest::prelude::*;

fn meter() -> impl Strategy<Value = Meter> {
    prop::collection::vec(
        (
            0i64..200_000,
            1u8..16,
            prop::sample::select(vec![1u8, 2, 4, 8, 16, 32, 64]),
        ),
        0..5,
    )
    .prop_map(|pts| {
        Meter::new(
            pts.into_iter()
                .map(|(tick, numerator, denominator)| MeterPoint {
                    tick: TickTime(tick),
                    numerator,
                    denominator,
                }),
        )
    })
}

fn grid() -> impl Strategy<Value = Grid> {
    (
        prop_oneof![
            Just(Division::Bar),
            Just(Division::Beat),
            // Most of these do not divide the bar, which is where steps
            // become fractional and swing is rounded.
            (1u16..=4096).prop_map(Division::Note),
            prop::sample::select(vec![1u16, 2, 4, 8, 16, 32, 64]).prop_map(Division::Note),
        ],
        prop::sample::select(vec![Feel::Straight, Feel::Triplet, Feel::Dotted]),
        prop_oneof![Just(0.0), 0.0..=1.0f64],
    )
        .prop_map(|(division, feel, swing)| Grid {
            division,
            feel,
            swing,
        })
}

proptest! {
    #[test]
    fn floor_next_bracket_and_snap_lands_on_a_line(
        m in meter(), g in grid(), t in -50_000i64..400_000,
    ) {
        let t = TickTime(t);
        let f = g.floor(t, &m);
        let n = g.next(t, &m);
        prop_assert!(f <= t && t < n, "{f:?} {t:?} {n:?}");
        // Nothing lies between floor and next.
        prop_assert_eq!(g.next(f, &m), n);
        prop_assert_eq!(g.floor(f, &m), f);
        let s = g.snap(t, &m);
        prop_assert!(s == f || s == n);
        prop_assert_eq!(g.snap(s, &m), s);
        // Nearest line, ties to the later one.
        prop_assert!(2 * (s.0 - t.0).abs() <= n.0 - f.0);
        if 2 * (t.0 - f.0) == n.0 - f.0 {
            prop_assert_eq!(s, n);
        }
        // The last line of a bar is followed by the next downbeat.
        let bar = m.bar_at(t);
        prop_assert_eq!(g.next(g.prev(bar.end(), &m), &m), bar.end());
    }

    #[test]
    fn lines_are_increasing_and_match_next(
        m in meter(), g in grid(), start in 0i64..300_000, len in 1i64..20_000,
    ) {
        let r = TickTime(start)..TickTime(start + len);
        let lines: Vec<_> = g.lines(r.clone(), &m).collect();
        prop_assert_eq!(lines.is_empty(), g.ceil(r.start, &m) >= r.end);
        for w in lines.windows(2) {
            prop_assert!(w[0].tick < w[1].tick);
            prop_assert_eq!(g.next(w[0].tick, &m), w[1].tick);
        }
        if let Some(first) = lines.first() {
            prop_assert_eq!(first.tick, g.ceil(r.start, &m));
        }
        if let Some(last) = lines.last() {
            prop_assert!(g.next(last.tick, &m) >= r.end);
        }
        prop_assert!(lines.iter().all(|l| r.contains(&l.tick)));
        for l in &lines {
            prop_assert_eq!(l.kind == LineKind::Bar, m.bar_at(l.tick).start == l.tick);
        }
    }

    #[test]
    fn results_near_the_edges_stay_in_range(
        m in meter(), g in grid(), d in 0i64..20_000,
    ) {
        let hi = TickTime(MAX_TICK - d);
        let n = g.next(hi, &m);
        prop_assert!(n.0 <= MAX_TICK);
        prop_assert_eq!(g.floor(n, &m), n);
        let lo = TickTime(-MAX_TICK + d);
        let p = g.prev(lo, &m);
        prop_assert!(p.0 >= -MAX_TICK);
        prop_assert_eq!(g.floor(p, &m), p);
        if p.0 > -MAX_TICK {
            prop_assert_eq!(g.next(p, &m), g.ceil(lo, &m));
        }
    }

    #[test]
    fn bar_start_inverts_bar_at(m in meter(), t in -50_000i64..400_000) {
        let bar = m.bar_at(TickTime(t));
        prop_assert_eq!(m.bar_start(bar.index), bar.start);
        prop_assert!(bar.start.0 <= t && t < bar.end().0);
        prop_assert_eq!(m.bar_at(bar.end()).index, bar.index + 1);
    }
}
