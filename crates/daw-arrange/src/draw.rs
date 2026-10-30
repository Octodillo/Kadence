//! Builds the arrange view's display list.

use daw_core::time::{TempoMap, TickTime};
use daw_midi::grid::{LineKind, Meter};
use daw_model::{AudioClip, Clip, ClipContent, Project, Track};

use crate::scene::{Align, Palette, Prim, Rect, Rgba, Scene, TRACK_COLORS};
use crate::snap::{MIN_LINE_SPACING, adaptive_grid, bar_stride};
use crate::{Arrange, Env, Peaks};

/// Bar numbers closer together than this are thinned out.
const LABEL_SPACING: f64 = 48.0;
const CLIP_RADIUS: f64 = 3.0;
/// Segments a fade curve is drawn with.
const FADE_SEGMENTS: usize = 24;
/// Clips shorter than this get no name header.
const HEADER_MIN_HEIGHT: f64 = 28.0;
const HEADER_HEIGHT: f64 = 16.0;

/// The colour a track's clips are drawn in.
pub fn track_color(track: &Track, row: usize) -> Rgba {
    match track.color {
        Some(c) => Rgba::rgb(c.0, c.1, c.2),
        None => TRACK_COLORS[row % TRACK_COLORS.len()],
    }
}

pub(crate) fn scene(a: &mut Arrange, project: &Project, env: &Env) -> Scene {
    let pal = a.palette;
    let (w, h, rh) = (a.width, a.height, a.ruler_height);
    let mut s = Scene::default();
    s.rect(Rect::new(0.0, 0.0, w, h), pal.background);
    let meter = a.meter(project);
    let lanes = Rect::new(0.0, rh, w, (h - rh).max(0.0));

    // Track lanes, alternating like a list view, the selected one tinted.
    let rows = project.tracks.len();
    let first = (a.scroll_y / a.track_height).floor().max(0.0) as usize;
    let last = (((a.scroll_y + lanes.h) / a.track_height).ceil() as usize).min(rows);
    let line = pal.background.mix(pal.text, 0.15);
    for row in first..last {
        let y = a.row_y(row);
        let fill = if a.selected_track == Some(project.tracks[row].id) {
            pal.background.mix(pal.highlight, 0.12)
        } else if row % 2 == 1 {
            pal.alternate_background
        } else {
            pal.background
        };
        s.rect(Rect::new(0.0, y, w, a.track_height), fill);
        s.hline(y + a.track_height - 0.5, 0.0, w, line, 1.0);
    }

    let loop_x = env
        .loop_range
        .map(|(l0, l1)| (a.view.tick_to_x(l0), a.view.tick_to_x(l1)));
    if let Some((x0, x1)) = loop_x {
        s.rect(
            Rect::new(x0, rh, x1 - x0, lanes.h).intersect(&lanes),
            pal.highlight.with_alpha(0.07),
        );
    }

    grid_lines(&mut s, a, &meter, lanes, pal.background, pal.text);

    s.push(Prim::PushClip(lanes));
    let range = a.view.visible(w);
    for row in first..last {
        let track = &project.tracks[row];
        let color = track_color(track, row);
        for clip in &track.clips {
            if clip.start >= range.end || clip.start.0 + clip.length.0 <= range.start.0 {
                continue;
            }
            draw_clip(&mut s, a, env, row, clip, color);
        }
    }
    if let Some(r) = a.rubber_band() {
        s.push(Prim::RoundRect {
            r,
            radius: 0.0,
            fill: pal.highlight.with_alpha(0.15),
            stroke: pal.highlight,
            stroke_width: 1.0,
        });
    }
    s.push(Prim::PopClip);

    // The ruler: bar numbers over the lanes, with the loop range in it.
    let ruler = Rect::new(0.0, 0.0, w, rh);
    s.rect(ruler, pal.alternate_background);
    if let Some((x0, x1)) = loop_x {
        s.rect(
            Rect::new(x0, 0.0, x1 - x0, rh).intersect(&ruler),
            pal.highlight.with_alpha(0.35),
        );
        for x in [x0, x1] {
            s.vline(x, 0.0, h, pal.highlight.with_alpha(0.7), 1.0);
        }
    }
    ruler_labels(&mut s, a, &meter, rh, pal.text);
    s.hline(rh - 0.5, 0.0, w, line, 1.0);

    let x = a.view.tick_to_x(env.playhead);
    if (-2.0..w + 2.0).contains(&x) {
        s.vline(x, 0.0, h, pal.highlight, 2.0);
    }
    s
}

fn grid_lines(s: &mut Scene, a: &Arrange, meter: &Meter, lanes: Rect, bg: Rgba, text: Rgba) {
    let ppt = a.view.px_per_tick();
    let range = a.view.visible(a.width);
    let color = |k: LineKind| match k {
        LineKind::Bar => bg.mix(text, 0.22),
        LineKind::Beat => bg.mix(text, 0.12),
        LineKind::Step => bg.mix(text, 0.06),
    };
    match adaptive_grid(ppt, MIN_LINE_SPACING) {
        Some(grid) => {
            for l in grid.lines(range, meter) {
                // A grid line is a bar line even if the grid is finer.
                let kind = if meter.bar_at(l.tick).start == l.tick {
                    LineKind::Bar
                } else {
                    l.kind
                };
                let x = a.view.tick_to_x(l.tick).round() + 0.5;
                s.vline(x, lanes.y, lanes.bottom(), color(kind), 1.0);
            }
        }
        None => {
            let stride = bar_stride(ppt, MIN_LINE_SPACING, meter, range.start);
            for t in bars(meter, range, stride) {
                let x = a.view.tick_to_x(t).round() + 0.5;
                s.vline(x, lanes.y, lanes.bottom(), color(LineKind::Bar), 1.0);
            }
        }
    }
}

/// Bar starts in `range`, every `stride` bars counting from bar 0.
fn bars(meter: &Meter, range: std::ops::Range<TickTime>, stride: i64) -> Vec<TickTime> {
    let mut out = Vec::new();
    let mut index = meter.bar_at(range.start).index;
    index -= index.rem_euclid(stride);
    loop {
        let t = meter.bar_start(index);
        if t >= range.end || out.len() > 10_000 {
            break;
        }
        if t >= range.start {
            out.push(t);
        }
        index += stride;
    }
    out
}

fn ruler_labels(s: &mut Scene, a: &Arrange, meter: &Meter, rh: f64, text: Rgba) {
    let ppt = a.view.px_per_tick();
    let mut range = a.view.visible(a.width);
    // Include the bar that starts just off the left edge so its number shows.
    range.start = meter.bar_at(range.start).start;
    let stride = bar_stride(ppt, LABEL_SPACING, meter, range.start);
    for t in bars(meter, range, stride) {
        let x = a.view.tick_to_x(t);
        let bar = meter.bar_at(t);
        s.vline(x.round() + 0.5, rh * 0.5, rh, text.with_alpha(0.5), 1.0);
        s.push(Prim::Text {
            r: Rect::new(x + 3.0, 0.0, LABEL_SPACING * stride as f64, rh),
            text: (bar.index + 1).to_string(),
            color: text,
            bold: false,
            align: Align::Left,
        });
    }
}

fn draw_clip(s: &mut Scene, a: &mut Arrange, env: &Env, row: usize, clip: &Clip, color: Rgba) {
    let pal = a.palette;
    let r = a.clip_rect(row, clip);
    let selected = a.selection.contains(&clip.id);
    let tint = if clip.muted { pal.disabled_text } else { color };
    let fill = pal.background.mix(tint, 0.30);
    let stroke = if selected {
        pal.highlight
    } else {
        pal.background.mix(tint, 0.85)
    };
    s.push(Prim::RoundRect {
        r,
        radius: CLIP_RADIUS,
        fill,
        stroke,
        stroke_width: if selected { 2.0 } else { 1.0 },
    });
    let mut body = Rect::new(r.x + 1.0, r.y + 1.0, r.w - 2.0, r.h - 2.0);
    if body.w < 2.0 || body.h < 2.0 {
        return;
    }
    s.push(Prim::PushClip(body));
    if r.h >= HEADER_MIN_HEIGHT {
        let header = Rect::new(body.x, body.y, body.w, HEADER_HEIGHT);
        s.rect(header, tint.mix(pal.background, 0.15));
        let on_tint = if tint.luminance() > 0.6 {
            Rgba::rgb(0x23, 0x26, 0x27)
        } else {
            Rgba::rgb(0xff, 0xff, 0xff)
        };
        // Keep the name visible when the clip starts off the left edge, and
        // clear of the fade-in handle on audio clips (always, so it doesn't
        // shift when the handles appear).
        let handle = if a.handles(row, clip).is_some() {
            a.handle_px + 2.0
        } else {
            0.0
        };
        let tx = header.x.max(0.0) + 4.0 + handle;
        s.text(
            Rect::new(tx, header.y, header.right() - tx - 2.0, header.h),
            &clip.name,
            on_tint,
            false,
        );
        body.y += HEADER_HEIGHT;
        body.h -= HEADER_HEIGHT;
    }
    let ink = pal.text.mix(tint, 0.45);
    match &clip.content {
        ClipContent::Audio(audio) => {
            let path = env.resolve(&audio.source);
            if let Peaks::Ready(cache) = a.peaks.get(&path) {
                let gain = 10f64.powf(audio.gain_db / 20.0) as f32;
                waveform(s, a, env, clip, audio, &cache, body, ink, gain);
            }
            if let Some(h) = a.handles(row, clip) {
                fades(s, r, body, &h, audio, pal.background, ink);
                if a.active_handles(row, clip).is_some() {
                    s.pop_clip();
                    handles(s, &h, audio, pal, ink);
                    return;
                }
            }
        }
        ClipContent::Midi(m) => {
            if m.notes.is_empty() {
                s.pop_clip();
                return;
            }
            let lo = m.notes.iter().map(|n| n.key).min().unwrap_or(60);
            let hi = m.notes.iter().map(|n| n.key).max().unwrap_or(60);
            // At least an octave, so a few notes don't fill the clip.
            let span = f64::from((hi - lo).max(11) + 1);
            let lo = f64::from(lo) - (span - f64::from(hi - lo + 1)) / 2.0;
            let nh = (body.h / span).max(1.0);
            for n in &m.notes {
                let x0 = a.view.tick_to_x(TickTime(clip.start.0 + n.start.0));
                let x1 = a
                    .view
                    .tick_to_x(TickTime(clip.start.0 + n.start.0 + n.length.0));
                if x1 < 0.0 || x0 > a.width {
                    continue;
                }
                let y = body.bottom() - (f64::from(n.key) - lo + 1.0) * nh;
                s.rect(Rect::new(x0, y, (x1 - x0).max(1.0), nh.max(1.0)), ink);
            }
        }
    }
    s.pop_clip();
}

/// Fade curves over the waveform, with the faded-out part dimmed.
fn fades(
    s: &mut Scene,
    r: Rect,
    body: Rect,
    h: &crate::Handles,
    audio: &AudioClip,
    background: Rgba,
    ink: Rgba,
) {
    let dim = background.with_alpha(0.45);
    // (from x, to x, shape, rising)
    let curves = [
        (r.x, h.fade_in_x, audio.fade_in.shape, true),
        (h.fade_out_x, r.right(), audio.fade_out.shape, false),
    ];
    for (x0, x1, shape, rising) in curves {
        let w = x1 - x0;
        if w < 1.0 {
            continue;
        }
        let y_at = |i: usize| {
            let t = i as f32 / FADE_SEGMENTS as f32;
            let g = shape.gain(if rising { t } else { 1.0 - t });
            body.bottom() - f64::from(g) * body.h
        };
        let step = w / FADE_SEGMENTS as f64;
        for i in 0..FADE_SEGMENTS {
            let xa = x0 + i as f64 * step;
            // Dim above the curve: what the fade takes away.
            let mid = (y_at(i) + y_at(i + 1)) / 2.0;
            s.rect(Rect::new(xa, body.y, step, mid - body.y), dim);
            s.push(Prim::Line {
                x0: xa,
                y0: y_at(i),
                x1: xa + step,
                y1: y_at(i + 1),
                color: ink,
                width: 1.5,
            });
        }
    }
}

/// The fade and gain handles: small squares along the clip's top edge,
/// filled where they are set to something.
fn handles(s: &mut Scene, h: &crate::Handles, audio: &AudioClip, pal: Palette, ink: Rgba) {
    let set = [
        (h.fade_in, !audio.fade_in.is_none()),
        (h.fade_out, !audio.fade_out.is_none()),
        (h.gain, audio.gain_db != 0.0),
    ];
    for (r, active) in set {
        s.push(Prim::RoundRect {
            r,
            radius: 2.0,
            fill: if active {
                pal.highlight
            } else {
                pal.background
            },
            stroke: if active { pal.highlight } else { ink },
            stroke_width: 1.0,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn waveform(
    s: &mut Scene,
    a: &Arrange,
    env: &Env,
    clip: &Clip,
    audio: &AudioClip,
    cache: &daw_media::PeakCache,
    body: Rect,
    ink: Rgba,
    gain: f32,
) {
    let offset = audio.offset;
    // Only the visible part of the clip.
    let x0 = body.x.max(0.0).floor();
    let x1 = body.right().min(a.width).ceil();
    let width = (x1 - x0) as usize;
    if width == 0 || cache.channels == 0 {
        return;
    }
    let clip_start = env.tempo.ticks_to_samples(clip.start, env.sample_rate).0;
    let ratio = f64::from(cache.sample_rate) / env.sample_rate;
    let file_frame = |x: f64| {
        let t = a.view.x_to_tick(x);
        // The offset is in the file's own frames.
        let engine = env.tempo.ticks_to_samples(t, env.sample_rate).0 - clip_start;
        ((engine.max(0) as f64 * ratio) as i64 + offset.0).max(0) as u64
    };
    let (f0, full) = (file_frame(x0), file_frame(x1));
    // The file may end before the clip does.
    let f1 = full.min(cache.frames);
    if f1 <= f0 {
        return;
    }
    let drawn = ((f1 - f0) as f64 / (full - f0) as f64 * width as f64).round() as usize;
    let channels = cache.channels.min(2);
    let lane_h = body.h / channels as f64;
    let mut columns = Vec::new();
    for ch in 0..channels {
        cache.query(ch, f0, f1, drawn.max(1), &mut columns);
        // Drawn at the clip's gain so a gain change shows.
        if gain != 1.0 {
            for (lo, hi) in &mut columns {
                *lo = (*lo * gain).clamp(-1.0, 1.0);
                *hi = (*hi * gain).clamp(-1.0, 1.0);
            }
        }
        s.push(Prim::Wave {
            r: Rect::new(
                x0,
                body.y + ch as f64 * lane_h,
                columns.len() as f64,
                lane_h,
            ),
            columns: std::mem::take(&mut columns),
            color: ink,
        });
    }
}

impl Scene {
    fn pop_clip(&mut self) {
        self.push(Prim::PopClip);
    }
}
