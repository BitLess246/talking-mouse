//! The multi-track timeline: chapters, captions and zooms as draggable bars.

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::project::Project;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Track {
    Chapter,
    Caption,
    Zoom,
}

const TRACKS: [Track; 3] = [Track::Chapter, Track::Caption, Track::Zoom];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ItemRef {
    pub track: Track,
    pub index: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Move,
    Start,
    End,
}

#[derive(Clone, Copy)]
struct Drag {
    item: ItemRef,
    mode: Mode,
    grab_t: f64,
    orig: (f64, f64),
}

pub struct Timeline {
    /// Seconds at the left edge.
    pub offset: f64,
    /// Zoom level; `None` fits the whole video.
    pub px_per_sec: Option<f32>,
    drag: Option<Drag>,
    scrubbing: bool,
}

#[derive(Default)]
pub struct Output {
    pub seek: Option<f64>,
    pub select: Option<Option<ItemRef>>,
    /// An item drag began: snapshot for undo now.
    pub begin_edit: bool,
    pub changed: bool,
}

const LABEL_W: f32 = 84.0;
const RULER_H: f32 = 22.0;
const ROW_H: f32 = 34.0;
const EDGE: f32 = 6.0;
const MIN_LEN: f64 = 0.2;

pub fn height() -> f32 {
    RULER_H + ROW_H * TRACKS.len() as f32 + 8.0
}

fn span(p: &Project, item: ItemRef) -> (f64, f64) {
    match item.track {
        Track::Chapter => (p.chapters[item.index].start, p.chapters[item.index].end),
        Track::Caption => (p.captions[item.index].start, p.captions[item.index].end),
        Track::Zoom => (p.zooms[item.index].start, p.zooms[item.index].end),
    }
}

fn set_span(p: &mut Project, item: ItemRef, (s, e): (f64, f64)) {
    match item.track {
        Track::Chapter => {
            let c = &mut p.chapters[item.index];
            (c.start, c.end) = (s, e);
        }
        Track::Caption => {
            let c = &mut p.captions[item.index];
            (c.start, c.end) = (s, e);
        }
        Track::Zoom => {
            let z = &mut p.zooms[item.index];
            (z.start, z.end) = (s, e);
        }
    }
}

fn count(p: &Project, track: Track) -> usize {
    match track {
        Track::Chapter => p.chapters.len(),
        Track::Caption => p.captions.len(),
        Track::Zoom => p.zooms.len(),
    }
}

fn label(p: &Project, item: ItemRef) -> (String, bool) {
    match item.track {
        Track::Chapter => {
            let c = &p.chapters[item.index];
            (format!("{}  {}", p.chapter_number(item.index), c.title), c.title.trim().is_empty())
        }
        Track::Caption => {
            let c = &p.captions[item.index];
            let text = match (c.action.trim(), c.target.trim()) {
                ("", t) => t.to_string(),
                (a, "") => a.to_string(),
                (a, t) => format!("{a} › {t}"),
            };
            // Drafted captions with no target yet are flagged for attention.
            (text, c.target.trim().is_empty() && c.description.trim().is_empty())
        }
        Track::Zoom => (format!("{:.1}×", p.zooms[item.index].scale), false),
    }
}

impl Timeline {
    pub fn new() -> Self {
        Self { offset: 0.0, px_per_sec: None, drag: None, scrubbing: false }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Project,
        playhead: f64,
        selected: Option<ItemRef>,
        follow: bool,
    ) -> Output {
        let mut out = Output::default();
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height()), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals().clone();
        let lanes = Rect::from_min_max(Pos2::new(rect.left() + LABEL_W, rect.top()), rect.max);
        let duration = p.duration.max(0.1);

        let fit = lanes.width() / duration as f32;
        let pps = self.px_per_sec.unwrap_or(fit).max(fit);
        if follow {
            let visible = lanes.width() as f64 / pps as f64;
            if playhead < self.offset || playhead > self.offset + visible * 0.95 {
                self.offset = playhead - visible * 0.1;
            }
        }
        // Ctrl/⌘+wheel zooms around the pointer, plain wheel pans.
        let mut pps = pps;
        if let Some(hover) = response.hover_pos() {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
            if zoom != 1.0 {
                let anchor = self.offset + ((hover.x - lanes.left()) / pps) as f64;
                pps = (pps * zoom).clamp(fit, 2000.0);
                self.px_per_sec = Some(pps);
                self.offset = anchor - ((hover.x - lanes.left()) / pps) as f64;
            } else if scroll.x != 0.0 || scroll.y != 0.0 {
                let d = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                self.offset -= (d / pps) as f64;
            }
        }
        let max_offset = (duration - lanes.width() as f64 / pps as f64).max(0.0);
        self.offset = self.offset.clamp(0.0, max_offset);
        let offset = self.offset;
        let to_x = |t: f64| lanes.left() + ((t - offset) * pps as f64) as f32;
        let to_t = |x: f32| offset + ((x - lanes.left()) / pps) as f64;

        // Backgrounds and labels.
        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        let row_rect = |i: usize| {
            Rect::from_min_size(
                Pos2::new(rect.left(), rect.top() + RULER_H + ROW_H * i as f32),
                Vec2::new(rect.width(), ROW_H),
            )
        };
        for (i, track) in TRACKS.iter().enumerate() {
            let r = row_rect(i);
            if i % 2 == 0 {
                painter.rect_filled(r, 0.0, visuals.faint_bg_color);
            }
            let name = match track {
                Track::Chapter => "Chapters",
                Track::Caption => "Captions",
                Track::Zoom => "Zooms",
            };
            painter.text(
                Pos2::new(r.left() + 10.0, r.center().y),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(12.0),
                visuals.weak_text_color(),
            );
        }

        // Ruler ticks at a readable spacing.
        let step = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0]
            .into_iter()
            .find(|s| s * pps as f64 >= 70.0)
            .unwrap_or(600.0);
        let mut tick = (self.offset / step).floor() * step;
        while to_x(tick) < lanes.right() {
            let x = to_x(tick);
            if x >= lanes.left() {
                painter.line_segment(
                    [Pos2::new(x, rect.top() + RULER_H - 6.0), Pos2::new(x, rect.bottom())],
                    Stroke::new(1.0, visuals.faint_bg_color.gamma_multiply(2.0)),
                );
                painter.text(
                    Pos2::new(x + 3.0, rect.top() + 4.0),
                    Align2::LEFT_TOP,
                    fmt_time(tick),
                    FontId::monospace(10.5),
                    visuals.weak_text_color(),
                );
            }
            tick += step;
        }

        // Click markers on the captions lane.
        let cap_row = row_rect(1);
        for c in &p.clicks {
            let x = to_x(c.t + p.cursor_offset);
            if x >= lanes.left() && x <= lanes.right() {
                painter.line_segment(
                    [Pos2::new(x, cap_row.bottom() - 6.0), Pos2::new(x, cap_row.bottom() - 1.0)],
                    Stroke::new(2.0, Color32::from_rgb(0xF2, 0xB2, 0x3A)),
                );
            }
        }

        // Items.
        let item_rect = |p: &Project, item: ItemRef| {
            let (s, e) = span(p, item);
            let r = row_rect(TRACKS.iter().position(|t| *t == item.track).unwrap());
            Rect::from_min_max(
                Pos2::new(to_x(s), r.top() + 4.0),
                Pos2::new(to_x(e).max(to_x(s) + 3.0), r.bottom() - 4.0),
            )
        };
        let lanes_painter = painter.with_clip_rect(lanes);
        for track in TRACKS {
            for index in 0..count(p, track) {
                let item = ItemRef { track, index };
                let r = item_rect(p, item);
                if r.right() < lanes.left() || r.left() > lanes.right() {
                    continue;
                }
                let (text, needs_text) = label(p, item);
                let fill = match track {
                    Track::Chapter => Color32::from_rgb(0x1B, 0x2C, 0x4C),
                    Track::Caption if needs_text => Color32::from_rgb(0x8A, 0x6A, 0x2A),
                    Track::Caption => Color32::from_rgb(0xD9, 0x9A, 0x22),
                    Track::Zoom => Color32::from_rgb(0x2E, 0x6E, 0x6A),
                };
                lanes_painter.rect_filled(r, 4.0, fill);
                if selected == Some(item) {
                    lanes_painter.rect_stroke(r, 4.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
                }
                let text_color = if track == Track::Caption && !needs_text {
                    Color32::from_rgb(0x10, 0x17, 0x28)
                } else {
                    Color32::WHITE
                };
                lanes_painter.with_clip_rect(r.shrink(2.0).intersect(lanes)).text(
                    Pos2::new(r.left() + 7.0, r.center().y),
                    Align2::LEFT_CENTER,
                    if needs_text { format!("{text} …") } else { text },
                    FontId::proportional(12.0),
                    text_color,
                );
            }
        }

        // Playhead.
        let px = to_x(playhead);
        if px >= lanes.left() && px <= lanes.right() {
            painter.line_segment(
                [Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())],
                Stroke::new(2.0, Color32::from_rgb(0xFF, 0x4D, 0x4D)),
            );
            painter.circle_filled(Pos2::new(px, rect.top() + 4.0), 4.5, Color32::from_rgb(0xFF, 0x4D, 0x4D));
        }

        // Hit testing: selected item wins, then topmost.
        let hit = |p: &Project, pos: Pos2| -> Option<(ItemRef, Mode)> {
            if !lanes.contains(pos) || pos.y < rect.top() + RULER_H {
                return None;
            }
            let classify = |item: ItemRef| {
                let r = item_rect(p, item);
                if !r.expand2(Vec2::new(EDGE * 0.5, 0.0)).contains(pos) {
                    return None;
                }
                Some(if (pos.x - r.left()).abs() <= EDGE {
                    Mode::Start
                } else if (pos.x - r.right()).abs() <= EDGE {
                    Mode::End
                } else {
                    Mode::Move
                })
            };
            if let Some(sel) = selected.filter(|s| s.index < count(p, s.track)) {
                if let Some(m) = classify(sel) {
                    return Some((sel, m));
                }
            }
            TRACKS
                .iter()
                .flat_map(|&track| (0..count(p, track)).rev().map(move |index| ItemRef { track, index }))
                .find_map(|it| classify(it).map(|m| (it, m)))
        };

        if let Some(pos) = response.hover_pos() {
            match hit(p, pos) {
                Some((_, Mode::Start | Mode::End)) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                Some((_, Mode::Move)) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
                None => {}
            }
        }

        if response.drag_started() {
            // Hit-test where the button went down, not where the drag was
            // recognised a few pixels later (which can be past a bar's edge).
            let origin = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos());
            if let Some(pos) = origin {
                if let Some((item, mode)) = hit(p, pos) {
                    self.drag = Some(Drag { item, mode, grab_t: to_t(pos.x), orig: span(p, item) });
                    out.select = Some(Some(item));
                    out.begin_edit = true;
                } else if pos.x >= lanes.left() {
                    self.scrubbing = true;
                }
            }
        }
        if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                let t = to_t(pos.x);
                if let Some(d) = self.drag {
                    let (s, e) = d.orig;
                    let dt = t - d.grab_t;
                    let new = match d.mode {
                        Mode::Move => {
                            let dt = dt.clamp(-s, duration - e);
                            (s + dt, e + dt)
                        }
                        Mode::Start => ((s + dt).clamp(0.0, e - MIN_LEN), e),
                        Mode::End => (s, (e + dt).clamp(s + MIN_LEN, duration)),
                    };
                    set_span(p, d.item, new);
                    out.changed = true;
                    ui.ctx().set_cursor_icon(if d.mode == Mode::Move {
                        CursorIcon::Grabbing
                    } else {
                        CursorIcon::ResizeHorizontal
                    });
                } else if self.scrubbing {
                    out.seek = Some(t.clamp(0.0, duration));
                }
            }
        }
        if response.drag_stopped() {
            self.drag = None;
            self.scrubbing = false;
        }
        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                match hit(p, pos) {
                    Some((item, _)) => out.select = Some(Some(item)),
                    None if pos.x >= lanes.left() => {
                        out.select = Some(None);
                        out.seek = Some(to_t(pos.x).clamp(0.0, duration));
                    }
                    None => {}
                }
            }
        }
        out
    }
}

pub fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let m = (t / 60.0).floor();
    let s = t - m * 60.0;
    if s.fract().abs() < 1e-6 {
        format!("{}:{:02}", m as u32, s as u32)
    } else {
        format!("{}:{:04.1}", m as u32, s)
    }
}
