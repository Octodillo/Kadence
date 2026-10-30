//! Automation lanes: parameter curves the engine plays back while the
//! transport runs.
//!
//! A lane is turned into `ParamValue` events at exact frames. Changes happen
//! only at breakpoints and, inside a linear ramp, on a fixed grid of
//! [`RAMP_INTERVAL`] samples counted from project start. Because the grid is
//! absolute, a render is identical whatever block size drives it.

use daw_core::event::{Event, EventKind, EventList, ParamId};
use daw_core::time::SampleTime;

/// Spacing of the value updates inside a linear ramp, in samples.
pub const RAMP_INTERVAL: i64 = 32;

/// How the value moves from one point to the next.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Curve {
    /// Holds this point's value until the next point.
    #[default]
    Step,
    /// Moves in a straight line to the next point's value.
    Linear,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AutomationPoint {
    pub time: SampleTime,
    /// Plain (not normalized) parameter value.
    pub value: f64,
    /// Shape of the segment that starts at this point.
    pub curve: Curve,
}

/// Points sorted by time. Before the first point the lane holds the first
/// value; after the last it holds the last.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutomationLane {
    points: Vec<AutomationPoint>,
}

impl AutomationLane {
    /// Sorts `points` by time. Of several points at the same time, the last
    /// one given wins.
    pub fn new(mut points: Vec<AutomationPoint>) -> Self {
        points.sort_by_key(|p| p.time);
        let mut deduped: Vec<AutomationPoint> = Vec::with_capacity(points.len());
        for p in points {
            match deduped.last_mut() {
                Some(last) if last.time == p.time => *last = p,
                _ => deduped.push(p),
            }
        }
        Self { points: deduped }
    }

    pub fn points(&self) -> &[AutomationPoint] {
        &self.points
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The value the engine plays at `t`: ramps are quantized to the
    /// [`RAMP_INTERVAL`] grid, so this is exactly what a node receives.
    pub fn value_at(&self, t: SampleTime) -> Option<f64> {
        Some(self.value_in(self.segment(t.0)?, t.0))
    }

    /// The value at `t`, which lies in the segment starting at point `i`.
    fn value_in(&self, i: usize, t: i64) -> f64 {
        let p = &self.points[i];
        match (p.curve, self.points.get(i + 1)) {
            (Curve::Linear, Some(next)) if t > p.time.0 => {
                let grid = t.div_euclid(RAMP_INTERVAL) * RAMP_INTERVAL;
                interpolate(p, next, grid.max(p.time.0))
            }
            _ => p.value,
        }
    }

    /// Index of the point whose segment holds `t`, or 0 before the first
    /// point.
    fn segment(&self, t: i64) -> Option<usize> {
        if self.points.is_empty() {
            return None;
        }
        Some(
            self.points
                .partition_point(|p| p.time.0 <= t)
                .saturating_sub(1),
        )
    }

    /// Pushes events for the block starting at `start` that is `frames` long:
    /// the current value at frame 0, then every change inside the block.
    /// Stops once `out` holds `limit` events. RT-safe; events that don't fit
    /// are dropped.
    pub(crate) fn emit(
        &self,
        param: ParamId,
        start: i64,
        frames: usize,
        out: &mut EventList,
        limit: usize,
    ) {
        let Some(mut seg) = self.segment(start) else {
            return;
        };
        let end = start + frames as i64;
        let push = |out: &mut EventList, t: i64, value: f64| {
            if out.len() >= limit {
                return;
            }
            let kind = EventKind::ParamValue { param, value };
            let _ = out.push(Event {
                frame: (t - start) as u32,
                kind,
            });
        };
        push(out, start, self.value_in(seg, start));
        loop {
            let p = &self.points[seg];
            let next = self.points.get(seg + 1);
            // Grid updates inside a ramp, after `start` and before the next point.
            if let (Curve::Linear, Some(n)) = (p.curve, next) {
                let from = (start + 1).max(p.time.0 + 1);
                let mut t = from.div_euclid(RAMP_INTERVAL) * RAMP_INTERVAL;
                if t < from {
                    t += RAMP_INTERVAL;
                }
                while t < end.min(n.time.0) {
                    push(out, t, interpolate(p, n, t));
                    t += RAMP_INTERVAL;
                }
            }
            match next {
                Some(n) if n.time.0 < end => {
                    if n.time.0 > start {
                        push(out, n.time.0, n.value);
                    }
                    seg += 1;
                }
                _ => break,
            }
        }
    }
}

fn interpolate(a: &AutomationPoint, b: &AutomationPoint, t: i64) -> f64 {
    let span = (b.time.0 - a.time.0) as f64;
    let x = (t - a.time.0) as f64 / span;
    a.value + (b.value - a.value) * x
}

/// Records a parameter while the user moves it during playback ("touch"
/// writing), then merges the take into a lane.
///
/// Feed it the value whenever the control moves and on a regular clock while
/// it is held, then [`finish`](Self::finish) when the user lets go. The take
/// is thinned so that no recorded value is more than `tolerance` away from
/// the line through the points kept.
#[derive(Clone, Debug)]
pub struct AutomationRecorder {
    take: Vec<(i64, f64)>,
    tolerance: f64,
}

impl AutomationRecorder {
    pub fn new(tolerance: f64) -> Self {
        Self {
            take: Vec::new(),
            tolerance: tolerance.max(0.0),
        }
    }

    /// Records `value` at `t`. Times that go backwards (a loop wrapped, the
    /// user located) are ignored; finish the take and start another instead.
    pub fn record(&mut self, t: SampleTime, value: f64) {
        match self.take.last_mut() {
            Some(last) if t.0 < last.0 => {}
            Some(last) if t.0 == last.0 => last.1 = value,
            _ => self.take.push((t.0, value)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.take.is_empty()
    }

    /// The time range recorded so far.
    pub fn range(&self) -> Option<(SampleTime, SampleTime)> {
        let first = self.take.first()?;
        let last = self.take.last()?;
        Some((SampleTime(first.0), SampleTime(last.0)))
    }

    /// Replaces `lane` between the first and last recorded time with the
    /// take. After the take, the lane goes back to what it was: a point at
    /// the release time restores the old value. Returns `lane` unchanged if
    /// nothing was recorded.
    pub fn finish(self, lane: &AutomationLane) -> AutomationLane {
        let Some((start, end)) = self.range() else {
            return lane.clone();
        };
        let kept = thin(&self.take, self.tolerance);
        let mut points: Vec<AutomationPoint> = lane
            .points
            .iter()
            .filter(|p| p.time < start || p.time > end)
            .copied()
            .collect();
        let after = lane.value_at(SampleTime(end.0 + 1));
        // A ramp running into the take would now aim at the take's first
        // value; pin it on its old line just before the take.
        if let Some(i) = lane.segment(start.0 - 1)
            && let (Curve::Linear, Some(next)) = (lane.points[i].curve, lane.points.get(i + 1))
            && lane.points[i].time.0 < start.0 - 1
        {
            points.push(AutomationPoint {
                time: SampleTime(start.0 - 1),
                value: interpolate(&lane.points[i], next, start.0 - 1),
                curve: Curve::Step,
            });
        }
        points.extend(kept.iter().map(|&(t, value)| AutomationPoint {
            time: SampleTime(t),
            value,
            curve: Curve::Linear,
        }));
        // Snap back to the old curve right after the take.
        if let Some(old) = after
            && !lane.points.iter().any(|p| p.time.0 == end.0 + 1)
        {
            points.push(AutomationPoint {
                time: SampleTime(end.0 + 1),
                value: old,
                curve: lane
                    .segment(end.0 + 1)
                    .map_or(Curve::Step, |i| lane.points[i].curve),
            });
        }
        if let Some(last) = points.iter_mut().filter(|p| p.time == end).last() {
            // The take's last point holds until the restore point.
            last.curve = Curve::Step;
        }
        AutomationLane::new(points)
    }
}

/// Ramer–Douglas–Peucker: keeps the first and last point, and every point
/// needed so the rest are within `tolerance` of the polyline.
fn thin(points: &[(i64, f64)], tolerance: f64) -> Vec<(i64, f64)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (ta, va) = points[a];
        let (tb, vb) = points[b];
        let mut worst = (0.0, 0);
        for (i, &(t, v)) in points.iter().enumerate().take(b).skip(a + 1) {
            let line = if tb == ta {
                va
            } else {
                va + (vb - va) * (t - ta) as f64 / (tb - ta) as f64
            };
            let d = (v - line).abs();
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(t: i64, v: f64, curve: Curve) -> AutomationPoint {
        AutomationPoint {
            time: SampleTime(t),
            value: v,
            curve,
        }
    }

    fn events(lane: &AutomationLane, start: i64, frames: usize) -> Vec<(u32, f64)> {
        let mut out = EventList::with_capacity(256);
        lane.emit(7, start, frames, &mut out, usize::MAX);
        out.iter()
            .map(|e| match e.kind {
                EventKind::ParamValue { param: 7, value } => (e.frame, value),
                k => panic!("unexpected {k:?}"),
            })
            .collect()
    }

    #[test]
    fn steps_land_on_exact_frames() {
        let lane = AutomationLane::new(vec![pt(0, 1.0, Curve::Step), pt(100, 0.5, Curve::Step)]);
        assert_eq!(events(&lane, 64, 64), vec![(0, 1.0), (36, 0.5)]);
        assert_eq!(events(&lane, 128, 64), vec![(0, 0.5)]);
        assert_eq!(lane.value_at(SampleTime(-5)), Some(1.0));
        assert_eq!(lane.value_at(SampleTime(99)), Some(1.0));
        assert_eq!(lane.value_at(SampleTime(100)), Some(0.5));
    }

    #[test]
    fn ramps_follow_an_absolute_grid() {
        let lane = AutomationLane::new(vec![pt(0, 0.0, Curve::Linear), pt(128, 1.0, Curve::Step)]);
        assert_eq!(
            events(&lane, 0, 256),
            vec![(0, 0.0), (32, 0.25), (64, 0.5), (96, 0.75), (128, 1.0)]
        );
        // A block starting mid-ramp gets the held grid value at frame 0.
        assert_eq!(events(&lane, 40, 30), vec![(0, 0.25), (24, 0.5)]);
    }

    #[test]
    fn duplicate_times_keep_the_last_point() {
        let lane = AutomationLane::new(vec![pt(10, 1.0, Curve::Step), pt(10, 2.0, Curve::Step)]);
        assert_eq!(lane.points().len(), 1);
        assert_eq!(lane.value_at(SampleTime(10)), Some(2.0));
        assert_eq!(AutomationLane::default().value_at(SampleTime(0)), None);
    }

    #[test]
    fn recording_replaces_the_touched_range_and_restores_after() {
        let lane = AutomationLane::new(vec![pt(0, 1.0, Curve::Step), pt(5000, 0.2, Curve::Step)]);
        let mut rec = AutomationRecorder::new(0.01);
        // A straight fade from 1.0 to 0.0 between 1000 and 2000, sampled
        // densely: thinning keeps only its ends.
        for i in 0..=100 {
            rec.record(SampleTime(1000 + i * 10), 1.0 - i as f64 / 100.0);
        }
        rec.record(SampleTime(500), 9.0); // backwards: ignored
        let out = rec.finish(&lane);
        let times: Vec<i64> = out.points().iter().map(|p| p.time.0).collect();
        assert_eq!(times, vec![0, 1000, 2000, 2001, 5000]);
        assert_eq!(out.value_at(SampleTime(999)), Some(1.0));
        assert_eq!(out.value_at(SampleTime(2000)), Some(0.0));
        assert_eq!(
            out.value_at(SampleTime(2001)),
            Some(1.0),
            "old curve resumes"
        );
        assert_eq!(out.value_at(SampleTime(5000)), Some(0.2));
        let mid = out.value_at(SampleTime(1500)).unwrap();
        // Within the ramp grid (32 samples) of the exact 0.5.
        assert!((mid - 0.5).abs() < 0.04, "{mid}");
    }

    #[test]
    fn recording_keeps_corners_and_handles_empty_lanes() {
        let mut rec = AutomationRecorder::new(0.05);
        for (t, v) in [(0, 0.0), (100, 0.5), (200, 1.0), (300, 0.5), (400, 0.0)] {
            rec.record(SampleTime(t), v);
        }
        let out = rec.finish(&AutomationLane::default());
        let kept: Vec<(i64, f64)> = out.points().iter().map(|p| (p.time.0, p.value)).collect();
        assert_eq!(kept, vec![(0, 0.0), (200, 1.0), (400, 0.0)]);
        let empty = AutomationRecorder::new(0.1).finish(&AutomationLane::default());
        assert!(empty.is_empty());
    }

    #[test]
    fn recording_leaves_the_curve_before_the_take_alone() {
        let ramp = AutomationLane::new(vec![pt(0, 0.0, Curve::Linear), pt(4000, 1.0, Curve::Step)]);
        let mut rec = AutomationRecorder::new(0.01);
        rec.record(SampleTime(1000), 0.9);
        rec.record(SampleTime(2000), 0.9);
        let out = rec.finish(&ramp);
        for t in [0, 500, 960] {
            assert_eq!(
                out.value_at(SampleTime(t)),
                ramp.value_at(SampleTime(t)),
                "at {t}"
            );
        }
        assert_eq!(out.value_at(SampleTime(1500)), Some(0.9));
        assert_eq!(out.value_at(SampleTime(4000)), Some(1.0));

        // A lane whose first point comes after the take: nothing before it
        // to keep, the take simply starts the lane.
        let late = AutomationLane::new(vec![pt(5000, 0.2, Curve::Step)]);
        let mut rec = AutomationRecorder::new(0.01);
        rec.record(SampleTime(1000), 0.7);
        rec.record(SampleTime(2000), 0.7);
        let out = rec.finish(&late);
        assert_eq!(out.points()[0].time, SampleTime(1000));
        assert_eq!(out.value_at(SampleTime(2001)), Some(0.2));
    }
}
