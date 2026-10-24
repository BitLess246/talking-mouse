//! The desktop editor: preview, timeline, inspector, recording and export.

mod preview;
mod timeline;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui::{self, Color32, Key, RichText, Sense, Stroke, Vec2};

use crate::auto;
use crate::overlay::viewport_at;
use crate::project::{Anchor, Caption, Chapter, Project, Zoom};
use crate::record::{self, InputLog, RecordOptions, Recording};
use crate::render::{self, ExportOptions, Progress};
use crate::video::EncodeOptions;
use crate::{project_path_for, relative_source};

use preview::{Preview, Request};
use timeline::{fmt_time, ItemRef, Timeline, Track};

const ACCENT: Color32 = Color32::from_rgb(0xF2, 0xB2, 0x3A);
const ACTIONS: [&str; 8] = ["Click", "Type", "Drag", "Select", "Scroll", "Hover", "Press", ""];

pub fn run(path: Option<PathBuf>) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Talking Mouse")
            // Matches StartupWMClass in the Linux .desktop file.
            .with_app_id("talking-mouse")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 600.0])
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "Talking Mouse",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc);
            if let Some(path) = path {
                app.open(&path);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("editor failed to start: {e}"))
}

fn window_icon() -> egui::IconData {
    let png = include_bytes!("../../assets/icon/icon-256.png");
    let pixmap = tiny_skia::Pixmap::decode_png(png).expect("bundled icon");
    let rgba = pixmap.pixels().iter().flat_map(|p| {
        let c = p.demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    });
    egui::IconData { rgba: rgba.collect(), width: pixmap.width(), height: pixmap.height() }
}

struct Doc {
    project: Project,
    /// Where the project is (or will be) saved.
    path: PathBuf,
    source: PathBuf,
    saved: bool,
}

enum RecState {
    Idle,
    Recording(Recording),
    Finishing(Receiver<Result<(Project, InputLog, PathBuf)>>),
}

struct ExportJob {
    progress: Arc<Progress>,
    rx: Receiver<Result<()>>,
    output: PathBuf,
    started: Instant,
}

struct RecordSettings {
    fps: u32,
    audio: bool,
    auto_zoom: bool,
    zoom_scale: f32,
    minimize: bool,
}

struct App {
    doc: Option<Doc>,
    preview: Preview,
    texture: Option<egui::TextureHandle>,
    playhead: f64,
    playing: bool,
    play_from: f64,
    preview_dirty: bool,
    selected: Option<ItemRef>,
    picking_anchor: bool,
    timeline: Timeline,
    undo: Vec<Project>,
    redo: Vec<Project>,
    last_checkpoint: Instant,
    rec: RecState,
    rec_settings: RecordSettings,
    export: Option<ExportJob>,
    export_height: Option<u32>,
    export_path: String,
    export_crf: u32,
    status: String,
    confirm_close: bool,
    allow_close: bool,
    title: String,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = ACCENT.gamma_multiply(0.55);
        visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        visuals.hyperlink_color = ACCENT;
        ctx.set_visuals(visuals);
        ctx.global_style_mut(|s| s.spacing.item_spacing = Vec2::new(8.0, 6.0));

        Self {
            doc: None,
            preview: Preview::spawn(ctx.clone()),
            texture: None,
            playhead: 0.0,
            playing: false,
            play_from: 0.0,
            preview_dirty: true,
            selected: None,
            picking_anchor: false,
            timeline: Timeline::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            last_checkpoint: Instant::now() - Duration::from_secs(10),
            rec: RecState::Idle,
            rec_settings: RecordSettings { fps: 30, audio: false, auto_zoom: true, zoom_scale: 1.6, minimize: true },
            export: None,
            export_height: None,
            export_path: String::new(),
            export_crf: 18,
            status: match crate::video::check_ffmpeg() {
                Ok(()) => "Record your screen, or open a video / project to start.".into(),
                Err(e) => format!("{e:#}. Recording, preview and export need it."),
            },
            confirm_close: false,
            allow_close: false,
            title: String::new(),
        }
    }

    // -- documents --------------------------------------------------------

    fn open(&mut self, path: &Path) {
        let is_project = path.to_string_lossy().ends_with(".json");
        let result = if is_project {
            Project::load(path).map(|p| (p, path.to_path_buf(), true))
        } else {
            let project_file = project_path_for(path);
            if project_file.exists() {
                Project::load(&project_file).map(|p| (p, project_file, true))
            } else {
                crate::new_project(path, &project_file).map(|p| (p, project_file, false))
            }
        };
        match result {
            Ok((project, file, saved)) => {
                let source = project.source_path(Some(&file));
                self.status = format!("Opened {}", file.display());
                self.set_doc(Doc { project, path: file, source, saved });
            }
            Err(e) => self.status = format!("Could not open {}: {e:#}", path.display()),
        }
    }

    fn set_doc(&mut self, doc: Doc) {
        let name = doc.path.file_name().map_or("export".into(), |n| n.to_string_lossy().replace(".tmouse.json", ""));
        self.export_path = doc.path.with_file_name(format!("{name}-captioned.mp4")).display().to_string();
        self.doc = Some(doc);
        self.playhead = 0.0;
        self.playing = false;
        self.selected = None;
        self.undo.clear();
        self.redo.clear();
        self.timeline = Timeline::new();
        self.preview_dirty = true;
    }

    fn save(&mut self) {
        let Some(doc) = &mut self.doc else { return };
        doc.project.sort();
        match doc.project.save(&doc.path) {
            Ok(()) => {
                doc.saved = true;
                self.selected = None;
                self.status = format!("Saved {}", doc.path.display());
            }
            Err(e) => self.status = format!("Save failed: {e:#}"),
        }
    }

    /// Snapshot for undo. Rapid edits (typing, dragging) coalesce.
    fn checkpoint(&mut self, force: bool) {
        let Some(doc) = &mut self.doc else { return };
        doc.saved = false;
        self.preview_dirty = true;
        if force || self.last_checkpoint.elapsed() > Duration::from_millis(900) {
            self.undo.push(doc.project.clone());
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last_checkpoint = Instant::now();
    }

    fn undo(&mut self, redo: bool) {
        let Some(doc) = &mut self.doc else { return };
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        if let Some(p) = from.pop() {
            to.push(std::mem::replace(&mut doc.project, p));
            doc.saved = false;
            self.selected = None;
            self.preview_dirty = true;
            self.last_checkpoint = Instant::now() - Duration::from_secs(10);
        }
    }

    // -- preview ----------------------------------------------------------

    fn preview_size(p: &Project) -> (u32, u32) {
        let scale = (1280.0 / p.width as f64).min(720.0 / p.height as f64).min(1.0);
        let w = ((p.width as f64 * scale) as u32).max(2) & !1;
        let h = ((p.height as f64 * scale) as u32).max(2) & !1;
        (w, h)
    }

    fn request_preview(&mut self) {
        let Some(doc) = &self.doc else { return };
        let t = if self.playing { self.play_from } else { self.playhead };
        self.preview.request(Request {
            project: Arc::new(doc.project.clone()),
            source: doc.source.clone(),
            t,
            playing: self.playing,
            size: Self::preview_size(&doc.project),
        });
        self.preview_dirty = false;
    }

    fn seek(&mut self, t: f64) {
        let dur = self.doc.as_ref().map_or(0.0, |d| d.project.duration);
        self.playhead = t.clamp(0.0, dur);
        self.playing = false;
        self.preview_dirty = true;
    }

    fn toggle_play(&mut self) {
        let Some(doc) = &self.doc else { return };
        self.playing = !self.playing;
        if self.playing {
            if self.playhead >= doc.project.duration - 0.05 {
                self.playhead = 0.0;
            }
            self.play_from = self.playhead;
        }
        self.preview_dirty = true;
    }

    fn poll_preview(&mut self, ctx: &egui::Context) {
        while let Ok(frame) = self.preview.rx.try_recv() {
            if self.playing {
                self.playhead = frame.t;
            }
            if frame.ended {
                self.playing = false;
            }
            if let Some(image) = frame.image {
                match &mut self.texture {
                    Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR)),
                }
            }
        }
    }

    // -- recording & export ------------------------------------------------

    fn start_recording(&mut self, ctx: &egui::Context) {
        let output = record::default_output(None);
        let opts = RecordOptions {
            output,
            fps: self.rec_settings.fps,
            region: None,
            audio: self.rec_settings.audio,
            input: None,
        };
        match Recording::start(opts) {
            Ok(rec) => {
                self.playing = false;
                self.rec = RecState::Recording(rec);
                self.status = format!("Recording… press {} or Stop to finish", record::STOP_HOTKEY);
                if self.rec_settings.minimize {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                }
            }
            Err(e) => self.status = format!("Could not start recording: {e:#}"),
        }
    }

    fn stop_recording(&mut self) {
        let RecState::Recording(rec) = std::mem::replace(&mut self.rec, RecState::Idle) else { return };
        let (tx, rx) = mpsc::channel();
        let (auto_zoom, scale) = (self.rec_settings.auto_zoom, self.rec_settings.zoom_scale);
        std::thread::spawn(move || {
            let result = rec.finish().map(|(mut project, log)| {
                let video = project.source.clone();
                let file = project_path_for(&video);
                project.source = relative_source(&video, &file);
                auto::draft_captions(&mut project, &log);
                if auto_zoom {
                    auto::draft_zooms(&mut project, scale);
                }
                (project, log, file)
            });
            let _ = tx.send(result);
        });
        self.rec = RecState::Finishing(rx);
        self.status = "Finishing recording…".into();
    }

    fn poll_recording(&mut self, ctx: &egui::Context) {
        match &mut self.rec {
            RecState::Recording(rec) => {
                if rec.stop_requested() || !rec.is_running() {
                    self.stop_recording();
                }
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            RecState::Finishing(rx) => match rx.try_recv() {
                Ok(result) => {
                    self.rec = RecState::Idle;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    match result {
                        Ok((project, log, file)) => {
                            let source = project.source_path(Some(&file));
                            let saved = project.save(&file).is_ok();
                            self.status = format!(
                                "Recorded {:.1}s — {} clicks, {} keystrokes. Drafted {} captions: fill in what each one points at.",
                                project.duration,
                                log.clicks.len(),
                                log.keystrokes.len(),
                                project.captions.len()
                            );
                            self.set_doc(Doc { project, path: file, source, saved });
                            self.selected = self
                                .doc
                                .as_ref()
                                .filter(|d| !d.project.captions.is_empty())
                                .map(|_| ItemRef { track: Track::Caption, index: 0 });
                        }
                        Err(e) => self.status = format!("Recording failed: {e:#}"),
                    }
                }
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(100)),
                Err(mpsc::TryRecvError::Disconnected) => self.rec = RecState::Idle,
            },
            RecState::Idle => {}
        }
    }

    fn start_export(&mut self) {
        let Some(doc) = &self.doc else { return };
        let output = PathBuf::from(self.export_path.trim());
        if output.as_os_str().is_empty() {
            self.status = "Choose an output file first.".into();
            return;
        }
        let encode = EncodeOptions { crf: self.export_crf, ..Default::default() };
        let progress = Arc::new(Progress::default());
        let (tx, rx) = mpsc::channel();
        let (project, source) = (doc.project.clone(), doc.source.clone());
        let opts = ExportOptions { output: output.clone(), height: self.export_height, encode, start: 0.0, end: None };
        {
            let progress = progress.clone();
            std::thread::spawn(move || {
                let _ = tx.send(render::export(&project, &source, &opts, &progress));
            });
        }
        self.export = Some(ExportJob { progress, rx, output, started: Instant::now() });
    }

    fn poll_export(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.export else { return };
        match job.rx.try_recv() {
            Ok(result) => {
                self.status = match result {
                    Ok(()) => {
                        format!("Exported {} in {:.0}s", job.output.display(), job.started.elapsed().as_secs_f64())
                    }
                    Err(e) => format!("Export failed: {e:#}"),
                };
                self.export = None;
            }
            Err(_) => ctx.request_repaint_after(Duration::from_millis(200)),
        }
    }

    // -- editing helpers -------------------------------------------------

    fn add_item(&mut self, track: Track) {
        let Some(doc) = &self.doc else { return };
        let p = &doc.project;
        let t = self.playhead;
        let duration = p.duration;
        let end = |len: f64| (t + len).min(duration);
        let anchor = if p.has_cursor() {
            Anchor::Cursor
        } else {
            Anchor::Fixed { x: p.width as f32 * 0.5, y: p.height as f32 * 0.5 }
        };
        self.checkpoint(true);
        let p = &mut self.doc.as_mut().unwrap().project;
        let index = match track {
            Track::Caption => {
                p.captions.push(Caption {
                    start: t,
                    end: end(2.0),
                    action: "Click".into(),
                    target: "Button".into(),
                    description: String::new(),
                    anchor,
                });
                p.captions.len() - 1
            }
            Track::Chapter => {
                p.chapters.push(Chapter {
                    start: t,
                    end: end(6.0),
                    number: String::new(),
                    title: "New chapter".into(),
                    subtitle: String::new(),
                    narration: String::new(),
                });
                p.chapters.len() - 1
            }
            Track::Zoom => {
                p.zooms.push(Zoom { start: t, end: end(3.0), scale: 1.6, focus: anchor });
                p.zooms.len() - 1
            }
        };
        self.selected = Some(ItemRef { track, index });
        self.picking_anchor = track == Track::Caption && !p.has_cursor();
    }

    fn delete_selected(&mut self) {
        let Some(sel) = self.selected.take() else { return };
        self.checkpoint(true);
        let p = &mut self.doc.as_mut().unwrap().project;
        match sel.track {
            Track::Caption if sel.index < p.captions.len() => drop(p.captions.remove(sel.index)),
            Track::Chapter if sel.index < p.chapters.len() => drop(p.chapters.remove(sel.index)),
            Track::Zoom if sel.index < p.zooms.len() => drop(p.zooms.remove(sel.index)),
            _ => {}
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (space, del, left, right, home, save, undo, redo) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Home),
                cmd && i.key_pressed(Key::S),
                cmd && !i.modifiers.shift && i.key_pressed(Key::Z),
                cmd && (i.key_pressed(Key::Y) || (i.modifiers.shift && i.key_pressed(Key::Z))),
            )
        });
        let step = ctx.input(|i| if i.modifiers.shift { 1.0 } else { 0.0 });
        let fps = self.doc.as_ref().map_or(30.0, |d| d.project.fps);
        if space {
            self.toggle_play();
        }
        if del {
            self.delete_selected();
        }
        if left {
            self.seek(self.playhead - if step > 0.0 { step } else { 1.0 / fps });
        }
        if right {
            self.seek(self.playhead + if step > 0.0 { step } else { 1.0 / fps });
        }
        if home {
            self.seek(0.0);
        }
        if save {
            self.save();
        }
        if undo {
            self.undo(false);
        }
        if redo {
            self.undo(true);
        }
    }

    // -- UI -----------------------------------------------------------------

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Talking Mouse").strong().color(ACCENT));
            ui.separator();

            let busy = !matches!(self.rec, RecState::Idle);
            match &self.rec {
                RecState::Recording(rec) => {
                    let secs = rec.elapsed().as_secs();
                    let stop = egui::Button::new(
                        RichText::new(format!("Stop  {:02}:{:02}", secs / 60, secs % 60)).color(Color32::WHITE),
                    )
                    .fill(Color32::from_rgb(0xC6, 0x28, 0x28));
                    if ui.add(stop).clicked() {
                        self.stop_recording();
                    }
                }
                RecState::Finishing(_) => {
                    ui.spinner();
                    ui.label("Finishing…");
                }
                RecState::Idle => {
                    let rec_btn = egui::Button::new(RichText::new("Record").color(Color32::WHITE))
                        .fill(Color32::from_rgb(0xC6, 0x28, 0x28));
                    if ui.add(rec_btn).clicked() {
                        let ctx = ui.ctx().clone();
                        self.start_recording(&ctx);
                    }
                    ui.menu_button("Options", |ui| {
                        let s = &mut self.rec_settings;
                        ui.add(egui::Slider::new(&mut s.fps, 15..=60).text("fps"));
                        ui.checkbox(&mut s.audio, "Record microphone");
                        ui.checkbox(&mut s.minimize, "Minimise editor while recording");
                        ui.checkbox(&mut s.auto_zoom, "Draft zooms around activity");
                        ui.add_enabled(s.auto_zoom, egui::Slider::new(&mut s.zoom_scale, 1.1..=3.0).text("zoom"));
                        ui.label(RichText::new(format!("Stop anytime with {}", record::STOP_HOTKEY)).weak());
                    });
                }
            }
            ui.separator();

            if ui.add_enabled(!busy, egui::Button::new("Open…")).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Project or video", &["json", "mp4", "mkv", "mov", "webm", "avi"])
                    .pick_file()
                {
                    self.open(&path);
                }
            }
            let has_doc = self.doc.is_some();
            if ui.add_enabled(has_doc, egui::Button::new("Save")).on_hover_text("Ctrl+S").clicked() {
                self.save();
            }
            if ui.add_enabled(!self.undo.is_empty(), egui::Button::new("Undo")).on_hover_text("Undo (Ctrl+Z)").clicked()
            {
                self.undo(false);
            }
            if ui
                .add_enabled(!self.redo.is_empty(), egui::Button::new("Redo"))
                .on_hover_text("Redo (Ctrl+Shift+Z)")
                .clicked()
            {
                self.undo(true);
            }
            ui.separator();

            match &self.export {
                Some(job) => {
                    let (done, total) =
                        (job.progress.done.load(Ordering::Relaxed), job.progress.total.load(Ordering::Relaxed).max(1));
                    ui.add(
                        egui::ProgressBar::new(done as f32 / total as f32)
                            .desired_width(180.0)
                            .text(format!("Exporting {done}/{total}")),
                    );
                    if ui.button("Cancel").clicked() {
                        job.progress.cancel.store(true, Ordering::Relaxed);
                    }
                }
                None => {
                    ui.add_enabled_ui(has_doc && !busy, |ui| {
                        ui.menu_button(RichText::new("Export MP4").strong(), |ui| {
                            ui.set_min_width(380.0);
                            ui.label("Save as");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut self.export_path).desired_width(290.0));
                                if ui.button("Browse…").clicked() {
                                    let current = PathBuf::from(&self.export_path);
                                    let mut dialog = rfd::FileDialog::new().add_filter("MP4 video", &["mp4"]);
                                    if let Some(dir) = current.parent() {
                                        dialog = dialog.set_directory(dir);
                                    }
                                    if let Some(name) = current.file_name() {
                                        dialog = dialog.set_file_name(name.to_string_lossy());
                                    }
                                    if let Some(path) = dialog.save_file() {
                                        self.export_path = path.display().to_string();
                                    }
                                }
                            });
                            egui::Grid::new("export-opts").num_columns(2).show(ui, |ui| {
                                ui.label("Resolution");
                                egui::ComboBox::from_id_salt("export-height")
                                    .selected_text(self.export_height.map_or("Source".into(), |h| format!("{h}p")))
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.export_height, None, "Source");
                                        for h in [2160, 1440, 1080, 720] {
                                            ui.selectable_value(&mut self.export_height, Some(h), format!("{h}p"));
                                        }
                                    });
                                ui.end_row();
                                ui.label("Quality");
                                ui.add(egui::Slider::new(&mut self.export_crf, 12..=30).text("CRF, lower = better"));
                                ui.end_row();
                            });
                            if ui.button(RichText::new("Start export").strong()).clicked() {
                                self.start_export();
                                ui.close();
                            }
                        });
                    });
                }
            }
        });
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = &self.doc else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("Record a walkthrough, or open a video.\n\nEvery click, drag and typing burst becomes a caption that follows your mouse.").size(16.0).weak());
            });
            return;
        };
        let p = &doc.project;
        let avail = ui.available_size() - Vec2::new(0.0, 4.0);
        let aspect = p.width as f32 / p.height as f32;
        let size = if avail.x / avail.y > aspect {
            Vec2::new(avail.y * aspect, avail.y)
        } else {
            Vec2::new(avail.x, avail.x / aspect)
        };
        let Some(tex) = &self.texture else {
            ui.centered_and_justified(|ui| ui.spinner());
            return;
        };
        let resp = ui.vertical_centered(|ui| ui.add(egui::Image::new((tex.id(), size)).sense(Sense::click()))).inner;
        let rect = resp.rect;
        let view = viewport_at(p, self.playhead);
        let to_screen = |(x, y): (f32, f32)| {
            egui::pos2(
                rect.left() + (x - view.x) / view.w * rect.width(),
                rect.top() + (y - view.y) / view.h * rect.height(),
            )
        };

        // Mark fixed anchors of the selected item.
        let anchor = self.selected.and_then(|s| match s.track {
            Track::Caption => p.captions.get(s.index).map(|c| c.anchor),
            Track::Zoom => p.zooms.get(s.index).map(|z| z.focus),
            Track::Chapter => None,
        });
        if let Some(Anchor::Fixed { x, y }) = anchor {
            let pos = to_screen((x, y));
            let painter = ui.painter_at(rect);
            painter.circle_stroke(pos, 9.0, Stroke::new(2.0, ACCENT));
            painter.line_segment([pos - Vec2::X * 14.0, pos + Vec2::X * 14.0], Stroke::new(1.5, ACCENT));
            painter.line_segment([pos - Vec2::Y * 14.0, pos + Vec2::Y * 14.0], Stroke::new(1.5, ACCENT));
        }

        if self.picking_anchor {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(2.0, ACCENT), egui::StrokeKind::Outside);
            if resp.clicked() {
                if let (Some(pos), Some(sel)) = (resp.interact_pointer_pos(), self.selected) {
                    let x = view.x + (pos.x - rect.left()) / rect.width() * view.w;
                    let y = view.y + (pos.y - rect.top()) / rect.height() * view.h;
                    self.checkpoint(true);
                    let p = &mut self.doc.as_mut().unwrap().project;
                    let fixed = Anchor::Fixed { x, y };
                    match sel.track {
                        Track::Caption => p.captions[sel.index].anchor = fixed,
                        Track::Zoom => p.zooms[sel.index].focus = fixed,
                        Track::Chapter => {}
                    }
                }
                self.picking_anchor = false;
            }
        } else if resp.clicked() {
            self.toggle_play();
        }
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = &self.doc else { return };
        let dur = doc.project.duration;
        ui.horizontal(|ui| {
            if ui.button(if self.playing { "Pause" } else { "Play" }).on_hover_text("Space").clicked() {
                self.toggle_play();
            }
            ui.label(RichText::new(format!("{} / {}", fmt_time(self.playhead), fmt_time(dur))).monospace());
            ui.separator();
            ui.label("Add at playhead:");
            if ui.button("+ Caption").clicked() {
                self.add_item(Track::Caption);
            }
            if ui.button("+ Chapter").clicked() {
                self.add_item(Track::Chapter);
            }
            if ui.button("+ Zoom").clicked() {
                self.add_item(Track::Zoom);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Fit").clicked() {
                    self.timeline.px_per_sec = None;
                }
                ui.label(RichText::new("Ctrl+scroll to zoom").weak().small());
            });
        });
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = &mut self.doc else { return };
        let out = self.timeline.show(ui, &mut doc.project, self.playhead, self.selected, self.playing);
        if out.begin_edit {
            self.checkpoint(true);
        }
        if out.changed {
            self.checkpoint(false);
        }
        if let Some(sel) = out.select {
            self.selected = sel;
            self.picking_anchor = false;
            // Show the selected item: jump inside it if the playhead is elsewhere.
            if let (Some(item), None) = (sel, out.seek) {
                let p = &self.doc.as_ref().unwrap().project;
                let (s, e) = match item.track {
                    Track::Caption => (p.captions[item.index].start, p.captions[item.index].end),
                    Track::Chapter => (p.chapters[item.index].start, p.chapters[item.index].end),
                    Track::Zoom => (p.zooms[item.index].start, p.zooms[item.index].end),
                };
                if !self.playing && (self.playhead < s || self.playhead > e) && !out.begin_edit {
                    self.seek(s + ((e - s) * 0.5).min(0.6));
                }
            }
        }
        if let Some(t) = out.seek {
            self.seek(t);
        }
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = &mut self.doc else {
            ui.weak("Nothing open.");
            return;
        };
        let mut changed = false;
        let mut delete = false;
        let mut pick = false;
        let mut jump = None;
        let has_cursor = doc.project.has_cursor();
        let dur = doc.project.duration;
        let time = |ui: &mut egui::Ui, label: &str, v: &mut f64| {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.add(egui::DragValue::new(v).speed(0.02).range(0.0..=dur).max_decimals(2).suffix(" s")).changed()
            })
            .inner
        };
        let anchor_ui = |ui: &mut egui::Ui, a: &mut Anchor, w: u32, h: u32, pick: &mut bool| {
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label("Pinned to");
                let is_cursor = *a == Anchor::Cursor;
                if ui.add_enabled(has_cursor, egui::RadioButton::new(is_cursor, "Cursor")).clicked() && !is_cursor {
                    *a = Anchor::Cursor;
                    changed = true;
                }
                if ui.radio(!is_cursor, "Fixed point").clicked() && is_cursor {
                    *a = Anchor::Fixed { x: w as f32 * 0.5, y: h as f32 * 0.5 };
                    changed = true;
                }
            });
            if let Anchor::Fixed { x, y } = a {
                ui.horizontal(|ui| {
                    changed |= ui.add(egui::DragValue::new(x).prefix("x ").range(0.0..=w as f32)).changed();
                    changed |= ui.add(egui::DragValue::new(y).prefix("y ").range(0.0..=h as f32)).changed();
                    if ui.button("Pick on preview").clicked() {
                        *pick = true;
                    }
                });
            }
            changed
        };
        let (w, h) = (doc.project.width, doc.project.height);

        egui::ScrollArea::vertical().show(ui, |ui| match self.selected {
            Some(ItemRef { track: Track::Caption, index }) if index < doc.project.captions.len() => {
                ui.heading("Caption");
                let c = &mut doc.project.captions[index];
                changed |= time(ui, "Start", &mut c.start);
                changed |= time(ui, "End  ", &mut c.end);
                ui.add_space(4.0);
                ui.label("Action");
                ui.horizontal_wrapped(|ui| {
                    for a in ACTIONS {
                        let label = if a.is_empty() { "none" } else { a };
                        if ui.selectable_label(c.action == a, label).clicked() {
                            c.action = a.to_string();
                            changed = true;
                        }
                    }
                });
                changed |= ui.add(egui::TextEdit::singleline(&mut c.action).hint_text("custom verb")).changed();
                ui.label("Target (bold)");
                changed |= ui.add(egui::TextEdit::singleline(&mut c.target).hint_text("e.g. Regenerate grid model").desired_width(f32::INFINITY)).changed();
                ui.label("Description (optional second line)");
                changed |= ui.add(egui::TextEdit::multiline(&mut c.description).desired_rows(3).desired_width(f32::INFINITY)).changed();
                ui.add_space(4.0);
                changed |= anchor_ui(ui, &mut c.anchor, w, h, &mut pick);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Go to start").clicked() {
                        jump = Some(c.start + 0.3);
                    }
                    delete = ui.button("Delete").clicked();
                });
            }
            Some(ItemRef { track: Track::Chapter, index }) if index < doc.project.chapters.len() => {
                ui.heading("Chapter card");
                let c = &mut doc.project.chapters[index];
                changed |= time(ui, "Start", &mut c.start);
                changed |= time(ui, "End  ", &mut c.end);
                ui.label("Number (blank = automatic)");
                changed |= ui.add(egui::TextEdit::singleline(&mut c.number).hint_text(format!("{:02}", index + 1))).changed();
                ui.label("Title");
                changed |= ui.add(egui::TextEdit::singleline(&mut c.title).desired_width(f32::INFINITY)).changed();
                ui.label("Subtitle");
                changed |= ui.add(egui::TextEdit::singleline(&mut c.subtitle).hint_text("BAYS · STOREY HEIGHTS · ONE CLICK").desired_width(f32::INFINITY)).changed();
                ui.label("Narration (typed out above the card)");
                changed |= ui.add(egui::TextEdit::multiline(&mut c.narration).desired_rows(2).desired_width(f32::INFINITY)).changed();
                ui.add_space(8.0);
                delete = ui.button("Delete").clicked();
            }
            Some(ItemRef { track: Track::Zoom, index }) if index < doc.project.zooms.len() => {
                ui.heading("Zoom");
                let z = &mut doc.project.zooms[index];
                changed |= time(ui, "Start", &mut z.start);
                changed |= time(ui, "End  ", &mut z.end);
                changed |= ui.add(egui::Slider::new(&mut z.scale, 1.0..=4.0).text("scale")).changed();
                changed |= anchor_ui(ui, &mut z.focus, w, h, &mut pick);
                ui.add_space(8.0);
                delete = ui.button("Delete").clicked();
            }
            _ => {
                let p = &mut doc.project;
                ui.heading("Project");
                ui.label(RichText::new(format!("{}×{} · {:.0} fps · {}", p.width, p.height, p.fps, fmt_time(p.duration))).weak());
                ui.label(RichText::new(doc.path.display().to_string()).weak().small());
                ui.add_space(6.0);
                ui.label(RichText::new("Style").strong());
                egui::Grid::new("style").num_columns(2).show(ui, |ui| {
                    let s = &mut p.style;
                    ui.label("Callout");
                    changed |= ui.color_edit_button_srgb(&mut s.accent).changed();
                    ui.end_row();
                    ui.label("Callout text");
                    changed |= ui.color_edit_button_srgb(&mut s.ink).changed();
                    ui.end_row();
                    ui.label("Card");
                    changed |= ui.color_edit_button_srgb(&mut s.panel).changed();
                    ui.end_row();
                    ui.label("Card text");
                    changed |= ui.color_edit_button_srgb(&mut s.panel_text).changed();
                    ui.end_row();
                    ui.label("Watermark");
                    changed |= ui.add(egui::TextEdit::singleline(&mut s.watermark).hint_text("yourapp.com")).changed();
                    ui.end_row();
                    ui.label("Overlay size");
                    changed |= ui.add(egui::Slider::new(&mut s.ui_scale, 0.5..=2.0)).changed();
                    ui.end_row();
                    ui.label("Follow lag");
                    changed |= ui.add(egui::Slider::new(&mut s.follow_lag, 0.0..=0.6).suffix(" s")).changed();
                    ui.end_row();
                    ui.label("Click ripples");
                    changed |= ui.checkbox(&mut s.click_ripples, "").changed();
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Cursor track").strong());
                if p.has_cursor() {
                    ui.label(RichText::new(format!("{} samples, {} clicks", p.cursor.len(), p.clicks.len())).weak());
                    egui::Grid::new("cursor").num_columns(2).show(ui, |ui| {
                        ui.label("Sync offset");
                        changed |= ui.add(egui::DragValue::new(&mut p.cursor_offset).speed(0.005).range(-5.0..=5.0).suffix(" s")).on_hover_text("Shift if callouts lead or trail the pointer").changed();
                        ui.end_row();
                        ui.label("Scale");
                        changed |= ui.add(egui::DragValue::new(&mut p.cursor_scale).speed(0.01).range(0.25..=4.0)).on_hover_text("2.0 on Retina / HiDPI if callouts sit at half the pointer position").changed();
                        ui.end_row();
                    });
                    if ui.button("Draft zooms from captions").clicked() {
                        auto::draft_zooms(p, 1.6);
                        changed = true;
                    }
                } else {
                    ui.label(RichText::new("No cursor track (imported video). Captions are pinned to points you pick on the preview.").weak());
                }
                ui.add_space(10.0);
                ui.label(RichText::new("Shortcuts").strong());
                ui.label(RichText::new("Space play · Left/Right frame (Shift: 1 s) · Del remove · Ctrl+Z undo · Ctrl+S save\nClick a timeline bar to edit it; drag to move, drag edges to trim.").weak().small());
            }
        });

        if changed {
            self.checkpoint(false);
        }
        if pick {
            self.picking_anchor = true;
        }
        if let Some(t) = jump {
            self.seek(t);
        }
        if delete {
            self.delete_selected();
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.doc {
            Some(d) => format!(
                "{}{} — Talking Mouse",
                if d.saved { "" } else { "● " },
                d.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
            ),
            None => "Talking Mouse".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_preview(&ctx);
        self.poll_recording(&ctx);
        self.poll_export(&ctx);
        self.handle_keys(&ctx);

        // Drop files onto the window to open them.
        if let Some(path) = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf())) {
            self.open(&path);
        }

        if ctx.input(|i| i.viewport().close_requested())
            && !self.allow_close
            && self.doc.as_ref().is_some_and(|d| !d.saved)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.label(RichText::new(&self.status).small());
        });
        egui::Panel::bottom("timeline").resizable(false).show(ui, |ui| {
            ui.add_space(4.0);
            self.transport(ui);
            self.timeline_ui(ui);
            ui.add_space(4.0);
        });
        egui::Panel::right("inspector").default_size(330.0).resizable(true).show(ui, |ui| {
            ui.add_space(6.0);
            self.inspector(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| self.preview_ui(ui));

        if self.confirm_close {
            let modal = egui::Modal::new(egui::Id::unique("confirm-close")).show(&ctx, |ui| {
                ui.heading("Save changes before closing?");
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.save();
                        self.allow_close = true;
                    }
                    if ui.button("Discard").clicked() {
                        self.allow_close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_close = false;
                    }
                });
            });
            if modal.should_close() {
                self.confirm_close = false;
            }
            if self.allow_close {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        if self.preview_dirty {
            self.request_preview();
        }
        if self.playing {
            ctx.request_repaint();
        }
        self.update_title(&ctx);
    }
}
