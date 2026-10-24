//! Background thread that decodes and composites preview frames so the UI
//! never blocks on ffmpeg.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::overlay::{compose, pixmap_from_rgba};
use crate::project::Project;
use crate::video::{frame_at, FrameReader};

#[derive(Clone)]
pub struct Request {
    pub project: Arc<Project>,
    pub source: PathBuf,
    pub t: f64,
    pub playing: bool,
    pub size: (u32, u32),
}

pub struct Frame {
    pub t: f64,
    /// `None` when playback reached the end of the video.
    pub image: Option<egui::ColorImage>,
    /// Playback reached the end of the video.
    pub ended: bool,
}

pub struct Preview {
    tx: Sender<Request>,
    pub rx: Receiver<Frame>,
}

impl Preview {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, req_rx) = mpsc::channel::<Request>();
        let (frame_tx, rx) = mpsc::channel::<Frame>();
        thread::spawn(move || worker(req_rx, frame_tx, ctx));
        Self { tx, rx }
    }

    pub fn request(&self, r: Request) {
        let _ = self.tx.send(r);
    }
}

struct Cached {
    source: PathBuf,
    t: f64,
    size: (u32, u32),
    rgba: Vec<u8>,
}

fn worker(rx: Receiver<Request>, tx: Sender<Frame>, ctx: egui::Context) {
    let mut cache: Option<Cached> = None;
    let send = |t: f64, frame: Option<tiny_skia::Pixmap>, ended: bool| {
        let image = frame
            .map(|f| egui::ColorImage::from_rgba_premultiplied([f.width() as usize, f.height() as usize], f.data()));
        let ok = tx.send(Frame { t, image, ended }).is_ok();
        ctx.request_repaint();
        ok
    };

    let mut pending: Option<Request> = None;
    loop {
        let mut req = match pending.take() {
            Some(r) => r,
            None => match rx.recv() {
                Ok(r) => r,
                Err(_) => return,
            },
        };
        // Only the newest request matters.
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let (w, h) = req.size;

        if !req.playing {
            let hit = cache
                .as_ref()
                .is_some_and(|c| c.source == req.source && c.size == req.size && (c.t - req.t).abs() < 1e-4);
            if !hit {
                match frame_at(&req.source, req.t, w, h) {
                    Ok(rgba) => cache = Some(Cached { source: req.source.clone(), t: req.t, size: req.size, rgba }),
                    Err(e) => {
                        eprintln!("preview: {e:#}");
                        continue;
                    }
                }
            }
            let c = cache.as_ref().unwrap();
            let frame = compose(&req.project, req.t, &pixmap_from_rgba(c.rgba.clone(), w, h), w, h);
            if !send(req.t, Some(frame), false) {
                return;
            }
            continue;
        }

        // Playback: stream frames, paced to real time, until a new request
        // arrives. Edits made while playing just swap the project in.
        let fps = req.project.fps.clamp(1.0, 30.0);
        let mut reader = match FrameReader::open(&req.source, req.t, fps, w, h) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("preview: {e:#}");
                continue;
            }
        };
        let started = Instant::now();
        let mut project = req.project.clone();
        let mut buf = Vec::new();
        for i in 0u64.. {
            match rx.try_recv() {
                Ok(newer)
                    if newer.playing
                        && newer.source == req.source
                        && newer.size == req.size
                        && (newer.t - req.t).abs() < 1e-6 =>
                {
                    project = newer.project;
                }
                Ok(newer) => {
                    pending = Some(newer);
                    break;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return,
            }
            let t = req.t + i as f64 / fps;
            let more = reader.next_frame(&mut buf).unwrap_or(false);
            if !more || t > project.duration {
                let _ = send(t.min(project.duration), None, true);
                break;
            }
            let due = started + Duration::from_secs_f64(i as f64 / fps);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
            let frame = compose(&project, t, &pixmap_from_rgba(buf.clone(), w, h), w, h);
            cache = Some(Cached { source: req.source.clone(), t, size: req.size, rgba: buf.clone() });
            if !send(t, Some(frame), false) {
                return;
            }
        }
    }
}
