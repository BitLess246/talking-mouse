//! The on-disk project: a source video plus everything layered on top of it.
//!
//! All times are in seconds on the source video's timeline and all positions
//! are in source-video pixels.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::video::VideoInfo;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    /// Source video. Relative paths are resolved against the project file.
    pub source: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    #[serde(default)]
    pub has_audio: bool,

    /// Seconds added to every cursor/click timestamp to line them up with
    /// the video (screen grabbers start a little after the tracker).
    #[serde(default)]
    pub cursor_offset: f64,
    /// Multiplier from tracked cursor coordinates to video pixels (e.g. 2.0
    /// on a Retina display where the OS reports points, not pixels).
    #[serde(default = "one")]
    pub cursor_scale: f64,
    #[serde(default)]
    pub cursor: Vec<CursorSample>,
    #[serde(default)]
    pub clicks: Vec<Click>,

    #[serde(default)]
    pub captions: Vec<Caption>,
    #[serde(default)]
    pub chapters: Vec<Chapter>,
    #[serde(default)]
    pub zooms: Vec<Zoom>,
    #[serde(default)]
    pub style: Style,
}

fn one() -> f64 {
    1.0
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CursorSample {
    pub t: f64,
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Click {
    pub t: f64,
    pub x: f32,
    pub y: f32,
}

/// Where an overlay is pinned.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Anchor {
    /// Follow the recorded mouse cursor.
    Cursor,
    /// A fixed point in the source video (for footage with no cursor track).
    Fixed { x: f32, y: f32 },
}

/// The amber "Click › Regenerate grid model" box that rides along with the
/// mouse.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Caption {
    pub start: f64,
    pub end: f64,
    /// Verb shown before the chevron ("Click", "Type", "Drag"). May be empty.
    #[serde(default)]
    pub action: String,
    /// What is being acted on. Shown in bold.
    #[serde(default)]
    pub target: String,
    /// Optional smaller second line.
    #[serde(default)]
    pub description: String,
    #[serde(default = "cursor_anchor")]
    pub anchor: Anchor,
}

fn cursor_anchor() -> Anchor {
    Anchor::Cursor
}

/// The numbered lower-third ("01  SET THE GRID").
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chapter {
    pub start: f64,
    pub end: f64,
    /// Badge text; when empty the chapter's 1-based position is used ("01").
    #[serde(default)]
    pub number: String,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    /// A line of narration typed out above the card.
    #[serde(default)]
    pub narration: String,
}

/// A smooth camera zoom that tracks its focus point.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Zoom {
    pub start: f64,
    pub end: f64,
    pub scale: f32,
    #[serde(default = "cursor_anchor")]
    pub focus: Anchor,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    /// Callout and badge fill.
    pub accent: [u8; 3],
    /// Text on the accent colour.
    pub ink: [u8; 3],
    /// Chapter card / watermark background.
    pub panel: [u8; 3],
    pub panel_text: [u8; 3],
    pub panel_muted: [u8; 3],
    /// Bottom-right pill; hidden when empty.
    pub watermark: String,
    /// Multiplier on every overlay's size (1.0 is tuned for 1080p).
    pub ui_scale: f32,
    /// Seconds the callout lags behind the cursor; higher is floatier.
    pub follow_lag: f32,
    /// Draw a ripple at every recorded click.
    pub click_ripples: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            accent: [0xF2, 0xB2, 0x3A],
            ink: [0x10, 0x17, 0x28],
            panel: [0x0D, 0x18, 0x2E],
            panel_text: [0xF4, 0xF6, 0xFA],
            panel_muted: [0x9A, 0xA8, 0xC0],
            watermark: String::new(),
            ui_scale: 1.0,
            follow_lag: 0.18,
            click_ripples: true,
        }
    }
}

impl Project {
    pub fn new(source: PathBuf, info: &VideoInfo) -> Self {
        Self {
            source,
            width: info.width,
            height: info.height,
            fps: info.fps,
            duration: info.duration,
            has_audio: info.has_audio,
            cursor_offset: 0.0,
            cursor_scale: 1.0,
            cursor: Vec::new(),
            clicks: Vec::new(),
            captions: Vec::new(),
            chapters: Vec::new(),
            zooms: Vec::new(),
            style: Style::default(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }

    /// The source video's path, resolved against the project file location.
    pub fn source_path(&self, project_file: Option<&Path>) -> PathBuf {
        match project_file.and_then(Path::parent) {
            Some(dir) if self.source.is_relative() => dir.join(&self.source),
            _ => self.source.clone(),
        }
    }

    pub fn has_cursor(&self) -> bool {
        !self.cursor.is_empty()
    }

    /// Raw cursor position at `t`, linearly interpolated.
    pub fn cursor_at(&self, t: f64) -> Option<(f32, f32)> {
        let t = t - self.cursor_offset;
        let s = &self.cursor;
        let first = s.first()?;
        let scale = self.cursor_scale as f32;
        let i = s.partition_point(|c| c.t <= t);
        let (x, y) = if i == 0 {
            (first.x, first.y)
        } else if i == s.len() {
            let l = s[s.len() - 1];
            (l.x, l.y)
        } else {
            let (a, b) = (s[i - 1], s[i]);
            let u = ((t - a.t) / (b.t - a.t).max(1e-9)) as f32;
            (a.x + (b.x - a.x) * u, a.y + (b.y - a.y) * u)
        };
        Some((x * scale, y * scale))
    }

    /// Cursor position smoothed over the trailing `lag` seconds. This is a
    /// pure function of `t` so preview scrubbing and export always agree.
    pub fn smoothed_cursor_at(&self, t: f64, lag: f32) -> Option<(f32, f32)> {
        if lag <= 0.0 {
            return self.cursor_at(t);
        }
        const STEPS: usize = 24;
        let window = lag as f64 * 2.5;
        let (mut sx, mut sy, mut sw) = (0.0f32, 0.0f32, 0.0f32);
        for k in 0..=STEPS {
            let dt = window * k as f64 / STEPS as f64;
            // Exponential decay weights approximate a critically damped follow.
            let w = (-(dt / lag as f64) as f32).exp();
            let (x, y) = self.cursor_at(t - dt)?;
            sx += x * w;
            sy += y * w;
            sw += w;
        }
        Some((sx / sw, sy / sw))
    }

    /// Resolves an anchor to a source-pixel position at `t`.
    pub fn anchor_at(&self, anchor: Anchor, t: f64, lag: f32) -> (f32, f32) {
        match anchor {
            Anchor::Fixed { x, y } => (x, y),
            Anchor::Cursor => {
                self.smoothed_cursor_at(t, lag).unwrap_or((self.width as f32 * 0.5, self.height as f32 * 0.5))
            }
        }
    }

    /// The badge text for chapter `index`.
    pub fn chapter_number(&self, index: usize) -> String {
        let c = &self.chapters[index];
        if c.number.is_empty() {
            format!("{:02}", index + 1)
        } else {
            c.number.clone()
        }
    }

    pub fn sort(&mut self) {
        let by_start = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
        self.captions.sort_by(|a, b| by_start(a.start, b.start));
        self.chapters.sort_by(|a, b| by_start(a.start, b.start));
        self.zooms.sort_by(|a, b| by_start(a.start, b.start));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        let info = VideoInfo { width: 100, height: 100, fps: 30.0, duration: 10.0, has_audio: false };
        let mut p = Project::new("x.mp4".into(), &info);
        p.cursor = vec![CursorSample { t: 0.0, x: 0.0, y: 0.0 }, CursorSample { t: 1.0, x: 10.0, y: 20.0 }];
        p
    }

    #[test]
    fn cursor_interpolates_and_clamps() {
        let p = project();
        assert_eq!(p.cursor_at(0.5), Some((5.0, 10.0)));
        assert_eq!(p.cursor_at(-1.0), Some((0.0, 0.0)));
        assert_eq!(p.cursor_at(5.0), Some((10.0, 20.0)));
    }

    #[test]
    fn cursor_offset_and_scale_apply() {
        let mut p = project();
        p.cursor_offset = 0.5;
        p.cursor_scale = 2.0;
        assert_eq!(p.cursor_at(1.0), Some((10.0, 20.0)));
    }

    #[test]
    fn smoothing_lags_behind() {
        let p = project();
        let (x, _) = p.smoothed_cursor_at(0.5, 0.2).unwrap();
        assert!(x < 5.0 && x > 2.0, "{x}");
        let (x, _) = p.smoothed_cursor_at(5.0, 0.2).unwrap();
        assert!((x - 10.0).abs() < 1e-3);
    }
}
