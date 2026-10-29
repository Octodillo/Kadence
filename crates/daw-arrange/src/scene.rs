//! A display list: what to draw, in view pixels and concrete colours, so a
//! front end only has to paint primitives.

/// A colour as `0xAARRGGBB`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgba(pub u32);

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    pub fn a(self) -> u8 {
        (self.0 >> 24) as u8
    }
    pub fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }
    pub fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }
    pub fn b(self) -> u8 {
        self.0 as u8
    }

    /// `self` blended towards `other` by `t` (0 is `self`, 1 is `other`).
    pub fn mix(self, other: Rgba, t: f64) -> Rgba {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as u32;
        Rgba(
            m(self.a(), other.a()) << 24
                | m(self.r(), other.r()) << 16
                | m(self.g(), other.g()) << 8
                | m(self.b(), other.b()),
        )
    }

    pub fn with_alpha(self, a: f64) -> Rgba {
        let a = (a.clamp(0.0, 1.0) * 255.0).round() as u32;
        Rgba(a << 24 | (self.0 & 0x00ff_ffff))
    }

    /// Relative luminance, 0 to 1.
    pub fn luminance(self) -> f64 {
        (0.2126 * f64::from(self.r()) + 0.7152 * f64::from(self.g()) + 0.0722 * f64::from(self.b()))
            / 255.0
    }
}

/// The colours a front end takes from its theme (on Plasma, Kirigami.Theme
/// in the View colour set).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Palette {
    pub background: Rgba,
    pub alternate_background: Rgba,
    pub text: Rgba,
    pub disabled_text: Rgba,
    pub highlight: Rgba,
    pub highlighted_text: Rgba,
    /// Breeze's "negative" colour, used for the playhead while recording
    /// (and nothing else yet).
    pub negative: Rgba,
}

impl Default for Palette {
    /// Breeze Light's view colours, for tests and headless rendering.
    fn default() -> Self {
        Self {
            background: Rgba::rgb(0xff, 0xff, 0xff),
            alternate_background: Rgba::rgb(0xf7, 0xf7, 0xf7),
            text: Rgba::rgb(0x23, 0x26, 0x27),
            disabled_text: Rgba::rgb(0xa0, 0xa2, 0xa4),
            highlight: Rgba::rgb(0x3d, 0xae, 0xe9),
            highlighted_text: Rgba::rgb(0xff, 0xff, 0xff),
            negative: Rgba::rgb(0xda, 0x44, 0x53),
        }
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    /// The rectangle spanned by two corners in any order.
    pub fn spanning(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self::new(x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs())
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        Rect::new(
            x,
            y,
            (self.right().min(o.right()) - x).max(0.0),
            (self.bottom().min(o.bottom()) - y).max(0.0),
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
}

/// One thing to paint. Later primitives paint over earlier ones.
#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    Rect {
        r: Rect,
        fill: Rgba,
    },
    RoundRect {
        r: Rect,
        radius: f64,
        fill: Rgba,
        stroke: Rgba,
        stroke_width: f64,
    },
    Line {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        color: Rgba,
        width: f64,
    },
    /// One line of text, elided to fit `r`.
    Text {
        r: Rect,
        text: String,
        color: Rgba,
        bold: bool,
        align: Align,
    },
    /// A waveform filling `r`: `columns` are (min, max) pairs in -1..1, one
    /// per pixel column from `r.x`.
    Wave {
        r: Rect,
        columns: Vec<(f32, f32)>,
        color: Rgba,
    },
    /// Clip everything until the matching `PopClip` to `r`.
    PushClip(Rect),
    PopClip,
}

/// What a front end paints for one frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub prims: Vec<Prim>,
}

impl Scene {
    pub fn push(&mut self, p: Prim) {
        self.prims.push(p);
    }

    pub fn rect(&mut self, r: Rect, fill: Rgba) {
        if r.w > 0.0 && r.h > 0.0 && fill.a() > 0 {
            self.push(Prim::Rect { r, fill });
        }
    }

    pub fn vline(&mut self, x: f64, y0: f64, y1: f64, color: Rgba, width: f64) {
        self.push(Prim::Line {
            x0: x,
            y0,
            x1: x,
            y1,
            color,
            width,
        });
    }

    pub fn hline(&mut self, y: f64, x0: f64, x1: f64, color: Rgba, width: f64) {
        self.push(Prim::Line {
            x0,
            y0: y,
            x1,
            y1: y,
            color,
            width,
        });
    }

    pub fn text(&mut self, r: Rect, text: impl Into<String>, color: Rgba, bold: bool) {
        self.push(Prim::Text {
            r,
            text: text.into(),
            color,
            bold,
            align: Align::Left,
        });
    }

    /// The `n`th text primitive's string, for tests.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.prims.iter().filter_map(|p| match p {
            Prim::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
    }
}

/// Default track colours when the model has none: Breeze's accent hues.
pub const TRACK_COLORS: [Rgba; 8] = [
    Rgba::rgb(0x3d, 0xae, 0xe9),
    Rgba::rgb(0x27, 0xae, 0x60),
    Rgba::rgb(0xf6, 0x74, 0x00),
    Rgba::rgb(0x9b, 0x59, 0xb6),
    Rgba::rgb(0xda, 0x44, 0x53),
    Rgba::rgb(0x1a, 0xbc, 0x9c),
    Rgba::rgb(0xfd, 0xbc, 0x4b),
    Rgba::rgb(0x2e, 0x86, 0xc1),
];
