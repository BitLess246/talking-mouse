//! Export: decode → composite (in parallel) → encode.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Result};

use crate::overlay::{compose, pixmap_from_rgba};
use crate::project::Project;
use crate::video::{fit_height, EncodeOptions, FrameReader, FrameWriter};

pub struct ExportOptions {
    pub output: PathBuf,
    /// Output height; width follows the source aspect ratio. `None` keeps
    /// the source size.
    pub height: Option<u32>,
    pub encode: EncodeOptions,
    /// Only export `[start, end)` seconds.
    pub start: f64,
    pub end: Option<f64>,
}

/// Shared progress for UIs: frames done out of total, plus a cancel flag.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

pub fn export(project: &Project, source: &Path, opts: &ExportOptions, progress: &Progress) -> Result<()> {
    let (out_w, out_h) = match opts.height {
        Some(h) => fit_height(project.width, project.height, h),
        None => (project.width & !1, project.height & !1),
    };
    let fps = project.fps;
    let end = opts.end.unwrap_or(project.duration).min(project.duration);
    let total = ((end - opts.start) * fps).ceil().max(0.0) as u64;
    progress.total.store(total, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);

    // Decode at no more than output size; there is no point decoding 4K for
    // a 1080p export with no zoom, but zoomed shots benefit from extra pixels.
    let (dec_w, dec_h) = if project.zooms.is_empty() && out_h < project.height {
        (out_w, out_h)
    } else {
        (project.width, project.height)
    };
    let mut reader = FrameReader::open(source, opts.start, fps, dec_w, dec_h)?;
    let audio = (project.has_audio && opts.start == 0.0 && opts.end.is_none()).then_some(source);
    let mut writer = FrameWriter::create(&opts.output, out_w, out_h, fps, audio, &opts.encode)?;

    let workers = thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let (job_tx, job_rx) = mpsc::sync_channel::<(u64, Vec<u8>)>(workers * 2);
    let (done_tx, done_rx) = mpsc::sync_channel::<(u64, Vec<u8>)>(workers * 2);
    let job_rx = Arc::new(Mutex::new(job_rx));
    let project = Arc::new(project.clone());

    let result = thread::scope(|s| -> Result<()> {
        for _ in 0..workers {
            let (job_rx, done_tx, project) = (job_rx.clone(), done_tx.clone(), project.clone());
            let start = opts.start;
            s.spawn(move || loop {
                let job = job_rx.lock().unwrap().recv();
                let Ok((i, rgba)) = job else { break };
                let t = start + i as f64 / fps;
                let frame = compose(&project, t, &pixmap_from_rgba(rgba, dec_w, dec_h), out_w, out_h);
                if done_tx.send((i, frame.take())).is_err() {
                    break;
                }
            });
        }
        drop(done_tx);

        let feeder = s.spawn(move || -> Result<()> {
            for i in 0..total {
                let mut buf = Vec::new();
                if progress.cancel.load(Ordering::Relaxed) || !reader.next_frame(&mut buf)? {
                    break;
                }
                if job_tx.send((i, buf)).is_err() {
                    break;
                }
            }
            Ok(())
        });

        // Workers finish out of order; write strictly in sequence.
        let mut pending = BTreeMap::new();
        let mut next = 0u64;
        for (i, frame) in done_rx {
            pending.insert(i, frame);
            while let Some(frame) = pending.remove(&next) {
                writer.write_frame(&frame)?;
                next += 1;
                progress.done.store(next, Ordering::Relaxed);
            }
        }
        feeder.join().map_err(|_| anyhow!("decoder thread panicked"))??;
        Ok(())
    });
    result?;
    if progress.cancel.load(Ordering::Relaxed) {
        return Err(anyhow!("export cancelled"));
    }
    writer.finish()
}

/// Renders one still (for thumbnails, docs and tests).
pub fn still(project: &Project, source: &Path, t: f64, out: &Path) -> Result<()> {
    let rgba = crate::video::frame_at(source, t, project.width, project.height)?;
    let frame =
        compose(project, t, &pixmap_from_rgba(rgba, project.width, project.height), project.width, project.height);
    frame.save_png(out).map_err(|e| anyhow!("writing {}: {e}", out.display()))
}
