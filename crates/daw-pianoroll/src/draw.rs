//! The piano roll's display list.

use daw_arrange::scene::Align;
use daw_arrange::snap::{MIN_LINE_SPACING, adaptive_grid};
use daw_arrange::{Env, Prim, Rect, Scene};
use daw_core::time::TickTime;
use daw_midi::EditNote;
use daw_midi::grid::LineKind;
use daw_midi::view::VelocityLane;
use daw_model::Project;

use crate::{PianoRoll, is_black, note_name};

const NOTE_RADIUS: f64 = 2.0;
const LABEL_SPACING: f64 = 48.0;

pub(crate) fn scene(pr: &PianoRoll, project: &Project, env: &Env) -> Scene {
    let pal = pr.palette;
    let (w, h) = (pr.width, pr.height);
    let mut s = Scene::default();
    s.rect(Rect::new(0.0, 0.0, w, h), pal.background);
    let n = pr.notes_rect();
    let vel = pr.velocity_rect();
    let line = pal.background.mix(pal.text, 0.15);
    let clip = pr.clip(project);
    let clip_start = clip.map_or(TickTime(0), |(c, _)| c.start);
    let v = pr.grid_view(clip_start);
    let meter = pr.meter(project);

    // Key rows: black keys darker, a stronger line under each C.
    s.push(Prim::PushClip(n));
    let rows = pr.visible_keys();
    for key in rows.clone() {
        let y = n.y + v.key_to_y(key);
        let row = Rect::new(n.x, y, n.w, pr.key_height);
        if is_black(key) {
            s.rect(row, pal.alternate_background.mix(pal.text, 0.04));
        }
        if key % 12 == 0 {
            s.hline(y + pr.key_height - 0.5, n.x, n.right(), line, 1.0);
        }
    }

    // Outside the clip is dimmed.
    if let Some((c, _)) = clip {
        let x0 = n.x + pr.view.tick_to_x(c.start);
        let x1 = n.x + pr.view.tick_to_x(TickTime(c.start.0 + c.length.0));
        let shade = pal.text.with_alpha(0.06);
        s.rect(Rect::new(n.x, n.y, (x0 - n.x).max(0.0), n.h), shade);
        s.rect(
            Rect::new(x1.max(n.x), n.y, (n.right() - x1).max(0.0), n.h),
            shade,
        );
    }

    // Grid lines.
    let ppt = pr.view.px_per_tick();
    let range = pr.view.visible(n.w);
    let color = |k: LineKind| match k {
        LineKind::Bar => pal.background.mix(pal.text, 0.25),
        LineKind::Beat => pal.background.mix(pal.text, 0.13),
        LineKind::Step => pal.background.mix(pal.text, 0.06),
    };
    if let Some(grid) = adaptive_grid(ppt, MIN_LINE_SPACING) {
        for l in grid.lines(range.clone(), &meter) {
            let kind = if meter.bar_at(l.tick).start == l.tick {
                LineKind::Bar
            } else {
                l.kind
            };
            let x = n.x + pr.view.tick_to_x(l.tick).round() + 0.5;
            s.vline(x, n.y, vel.bottom(), color(kind), 1.0);
        }
    }

    // Notes, selected ones in the highlight colour. Velocity sets opacity.
    if let Some((_, m)) = clip {
        for note in &m.notes {
            let d = note.data();
            if !rows.contains(&d.key) {
                continue;
            }
            let mut r = v.note_rect(&d);
            r.x += n.x;
            r.y += n.y;
            if r.x + r.w < n.x || r.x > n.right() {
                continue;
            }
            let r = Rect::new(r.x, r.y + 0.5, r.w.max(2.0), r.h - 1.0);
            let selected = pr.selection.contains(&note.id);
            let base = if selected {
                pal.highlight
            } else {
                pal.highlight.mix(pal.text, 0.35)
            };
            let fill = pal.background.mix(base, 0.45 + 0.55 * d.velocity);
            let stroke = if selected {
                pal.text
            } else {
                base.mix(pal.text, 0.3)
            };
            s.push(Prim::RoundRect {
                r,
                radius: NOTE_RADIUS,
                fill,
                stroke,
                stroke_width: 1.0,
            });
            if r.w > 28.0 && pr.key_height >= 12.0 {
                let text = if fill.luminance() > 0.55 {
                    pal.text
                } else {
                    pal.highlighted_text
                };
                s.text(
                    Rect::new(r.x + 3.0, r.y, r.w - 4.0, r.h),
                    note_name(d.key),
                    text,
                    false,
                );
            }
        }
    }
    if let Some(b) = pr.band() {
        s.push(Prim::RoundRect {
            r: b,
            radius: 0.0,
            fill: pal.highlight.with_alpha(0.2),
            stroke: pal.highlight,
            stroke_width: 1.0,
        });
    }
    s.push(Prim::PopClip);

    // Velocity lane.
    s.rect(vel, pal.alternate_background);
    s.hline(vel.y + 0.5, 0.0, w, line, 1.0);
    if let Some((_, m)) = clip {
        let lane = VelocityLane {
            height: vel.h - 4.0,
        };
        s.push(Prim::PushClip(vel));
        for note in &m.notes {
            let d = note.data();
            let x = n.x + v.note_rect(&d).x;
            if x < n.x - 4.0 || x > n.right() + 4.0 {
                continue;
            }
            let top = vel.y + 4.0 + lane.velocity_to_y(d.velocity);
            let selected = pr.selection.contains(&note.id);
            let c = if selected {
                pal.highlight
            } else {
                pal.text.with_alpha(0.55)
            };
            s.vline(x + 0.5, top, vel.bottom(), c, 2.0);
            s.rect(Rect::new(x - 2.0, top - 2.0, 5.0, 5.0), c);
        }
        s.push(Prim::PopClip);
    }

    // Keyboard strip.
    let kb = Rect::new(0.0, n.y, pr.keyboard_width, n.h);
    s.push(Prim::PushClip(kb));
    s.rect(kb, pal.background);
    for key in rows {
        let y = n.y + v.key_to_y(key);
        let r = Rect::new(0.0, y, pr.keyboard_width, pr.key_height);
        if is_black(key) {
            s.rect(
                Rect::new(0.0, y, pr.keyboard_width * 0.62, pr.key_height),
                pal.text.mix(pal.background, 0.15),
            );
        } else {
            s.hline(y + pr.key_height - 0.5, 0.0, pr.keyboard_width, line, 1.0);
        }
        if key % 12 == 0 && pr.key_height >= 9.0 {
            s.push(Prim::Text {
                r: Rect::new(0.0, r.y, pr.keyboard_width - 4.0, r.h),
                text: note_name(key),
                color: pal.disabled_text,
                bold: false,
                align: Align::Left,
            });
        }
    }
    s.push(Prim::PopClip);
    s.vline(pr.keyboard_width - 0.5, n.y, h, line, 1.0);

    // Ruler with bar numbers.
    let rh = pr.ruler_height;
    s.rect(Rect::new(0.0, 0.0, w, rh), pal.alternate_background);
    s.hline(rh - 0.5, 0.0, w, line, 1.0);
    s.push(Prim::PushClip(Rect::new(n.x, 0.0, n.w, rh)));
    let mut index = meter.bar_at(range.start).index;
    let px_per_bar = meter.bar_at(range.start).len as f64 * ppt;
    let stride = ((LABEL_SPACING / px_per_bar.max(1e-9))
        .ceil()
        .clamp(1.0, 1e6) as u64)
        .next_power_of_two() as i64;
    index -= index.rem_euclid(stride);
    for _ in 0..512 {
        let t = meter.bar_start(index);
        if t >= range.end {
            break;
        }
        let x = n.x + pr.view.tick_to_x(t);
        s.vline(x.round() + 0.5, rh * 0.5, rh, pal.text.with_alpha(0.5), 1.0);
        s.text(
            Rect::new(x + 3.0, 0.0, LABEL_SPACING * stride as f64, rh),
            (index + 1).to_string(),
            pal.text,
            false,
        );
        index += stride;
    }
    s.push(Prim::PopClip);

    // Playhead.
    let x = n.x + pr.view.tick_to_x(env.playhead);
    if (n.x - 2.0..w + 2.0).contains(&x) {
        s.vline(x, 0.0, vel.bottom(), pal.highlight, 2.0);
    }
    s
}

impl PianoRoll {
    /// Keys with a visible row, lowest first.
    pub fn visible_keys(&self) -> std::ops::RangeInclusive<u8> {
        let v = self.grid_view(TickTime(0));
        v.visible_keys(self.notes_rect().h)
    }
}
