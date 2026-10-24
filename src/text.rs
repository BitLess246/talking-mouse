//! Glyph layout and rasterisation straight into a `tiny_skia::Pixmap`.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use tiny_skia::Pixmap;

pub struct Fonts {
    /// Chapter titles and badge numbers.
    pub heavy: FontRef<'static>,
    /// Callout titles.
    pub bold: FontRef<'static>,
    /// Callout descriptions.
    pub medium: FontRef<'static>,
    /// Subtitles, narration and the watermark.
    pub mono: FontRef<'static>,
}

pub fn fonts() -> &'static Fonts {
    static FONTS: OnceLock<Fonts> = OnceLock::new();
    FONTS.get_or_init(|| {
        let load = |bytes: &'static [u8]| FontRef::try_from_slice(bytes).expect("bundled font");
        Fonts {
            heavy: load(include_bytes!("../assets/fonts/InterDisplay-ExtraBold.otf")),
            bold: load(include_bytes!("../assets/fonts/Inter-Bold.otf")),
            medium: load(include_bytes!("../assets/fonts/Inter-Medium.otf")),
            mono: load(include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf")),
        }
    })
}

/// A run of text at a given size and tracking.
#[derive(Clone, Copy)]
pub struct TextStyle<'a> {
    pub font: &'a FontRef<'static>,
    pub size: f32,
    /// Extra advance after each glyph, in pixels.
    pub tracking: f32,
}

impl<'a> TextStyle<'a> {
    pub fn new(font: &'a FontRef<'static>, size: f32) -> Self {
        Self { font, size, tracking: 0.0 }
    }

    pub fn tracking(mut self, px: f32) -> Self {
        self.tracking = px;
        self
    }

    /// ab_glyph scales by ascent-to-descent height; convert from em size so
    /// sizes mean what they do in CSS.
    fn scale(&self) -> PxScale {
        let upem = self.font.units_per_em().unwrap_or(1000.0);
        PxScale::from(self.size * self.font.height_unscaled() / upem)
    }

    pub fn ascent(&self) -> f32 {
        self.font.as_scaled(self.scale()).ascent()
    }

    pub fn descent(&self) -> f32 {
        self.font.as_scaled(self.scale()).descent()
    }

    /// Height of capital letters, for optically centring single lines.
    pub fn cap_height(&self) -> f32 {
        let scaled = self.font.as_scaled(self.scale());
        let id = self.font.glyph_id('H');
        let glyph = id.with_scale(self.scale());
        self.font.outline_glyph(glyph).map(|g| -g.px_bounds().min.y).unwrap_or(scaled.ascent() * 0.7)
    }

    pub fn width(&self, text: &str) -> f32 {
        let scaled = self.font.as_scaled(self.scale());
        let mut w = 0.0;
        let mut prev = None;
        for ch in text.chars() {
            let id = self.font.glyph_id(ch);
            if let Some(p) = prev {
                w += scaled.kern(p, id);
            }
            w += scaled.h_advance(id) + self.tracking;
            prev = Some(id);
        }
        if !text.is_empty() {
            w -= self.tracking;
        }
        w
    }

    /// Greedy word wrap to `max_width`.
    pub fn wrap(&self, text: &str, max_width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for para in text.split('\n') {
            let mut line = String::new();
            for word in para.split_whitespace() {
                let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                if !line.is_empty() && self.width(&candidate) > max_width {
                    lines.push(std::mem::replace(&mut line, word.to_string()));
                } else {
                    line = candidate;
                }
            }
            lines.push(line);
        }
        lines
    }

    /// Draws `text` with its baseline at `y`, returning the end x.
    pub fn draw(&self, pixmap: &mut Pixmap, text: &str, x: f32, y: f32, rgba: [u8; 4]) -> f32 {
        let scale = self.scale();
        let scaled = self.font.as_scaled(scale);
        let mut pen = x;
        let mut prev = None;
        for ch in text.chars() {
            let id = self.font.glyph_id(ch);
            if let Some(p) = prev {
                pen += scaled.kern(p, id);
            }
            let glyph = id.with_scale_and_position(scale, ab_glyph::point(pen, y));
            if let Some(outline) = self.font.outline_glyph(glyph) {
                let b = outline.px_bounds();
                outline.draw(|gx, gy, c| {
                    blend_pixel(pixmap, b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, rgba, c);
                });
            }
            pen += scaled.h_advance(id) + self.tracking;
            prev = Some(id);
        }
        pen
    }
}

/// Source-over blend of a straight-alpha colour with `coverage` into a
/// premultiplied pixmap.
pub fn blend_pixel(pixmap: &mut Pixmap, x: i32, y: i32, rgba: [u8; 4], coverage: f32) {
    if x < 0 || y < 0 || x >= pixmap.width() as i32 || y >= pixmap.height() as i32 {
        return;
    }
    let a = (rgba[3] as f32 / 255.0) * coverage.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let idx = (y as usize * pixmap.width() as usize + x as usize) * 4;
    let data = pixmap.data_mut();
    let inv = 1.0 - a;
    for k in 0..3 {
        let v = rgba[k] as f32 * a + data[idx + k] as f32 * inv;
        data[idx + k] = v.round().min(255.0) as u8;
    }
    data[idx + 3] = (255.0 * a + data[idx + 3] as f32 * inv).round().min(255.0) as u8;
}
