use std::io::BufRead;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use talking_mouse::project::Project;
use talking_mouse::record::{self, RecordOptions, Recording};
use talking_mouse::render::{self, ExportOptions, Progress};
use talking_mouse::video::EncodeOptions;
use talking_mouse::{auto, new_project, project_path_for, relative_source};

#[derive(Parser)]
#[command(version, about = "Screen recorder & editor that pins captions to your mouse cursor")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the editor (the default). Accepts a project file or a video.
    Edit { path: Option<PathBuf> },
    /// Record the screen plus cursor track; stop with Enter, Ctrl+C or Ctrl+Shift+F9.
    Record {
        /// Output video (.mkv recommended). Defaults to ~/Videos/talking-mouse-<time>.mkv
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        /// Region to capture as x,y,w,h
        #[arg(long, value_parser = parse_region)]
        region: Option<(i32, i32, u32, u32)>,
        /// Also record the default audio input
        #[arg(long)]
        audio: bool,
        /// Override the ffmpeg capture input (e.g. ":1.0", "2:none", "desktop")
        #[arg(long)]
        input: Option<String>,
        /// Draft zooms around each burst of activity, at this scale
        #[arg(long)]
        auto_zoom: Option<f32>,
    },
    /// Start a project from an existing video (captions are pinned to fixed points).
    New {
        video: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Export a project to MP4.
    Render {
        project: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Output height in pixels (width follows aspect). Defaults to source size.
        #[arg(long)]
        height: Option<u32>,
        #[arg(long, default_value_t = 18)]
        crf: u32,
        #[arg(long, default_value = "medium")]
        preset: String,
        #[arg(long, default_value_t = 0.0)]
        from: f64,
        #[arg(long)]
        to: Option<f64>,
    },
    /// Render a single frame to PNG.
    Still {
        project: PathBuf,
        #[arg(long)]
        at: f64,
        #[arg(short, long)]
        output: PathBuf,
    },
}

fn parse_region(s: &str) -> Result<(i32, i32, u32, u32), String> {
    let v: Vec<&str> = s.split(',').map(str::trim).collect();
    let err = || format!("expected x,y,w,h, got {s:?}");
    if v.len() != 4 {
        return Err(err());
    }
    Ok((
        v[0].parse().map_err(|_| err())?,
        v[1].parse().map_err(|_| err())?,
        v[2].parse().map_err(|_| err())?,
        v[3].parse().map_err(|_| err())?,
    ))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Cmd::Edit { path: None }) {
        Cmd::Edit { path } => run_editor(path),
        Cmd::Record { output, fps, region, audio, input, auto_zoom } => {
            let output = output.unwrap_or_else(|| record::default_output(None));
            let opts = RecordOptions { output: output.clone(), fps, region, audio, input };
            let mut rec = Recording::start(opts)?;
            eprintln!("● Recording to {} — press Enter, Ctrl+C or {} to stop", output.display(), record::STOP_HOTKEY);
            let stop = Arc::new(AtomicBool::new(false));
            {
                let on_ctrlc = stop.clone();
                ctrlc::set_handler(move || on_ctrlc.store(true, Ordering::Relaxed))?;
                let on_enter = stop.clone();
                std::thread::spawn(move || {
                    let _ = std::io::stdin().lock().read_line(&mut String::new());
                    on_enter.store(true, Ordering::Relaxed);
                });
            }
            while !stop.load(Ordering::Relaxed) && !rec.stop_requested() && rec.is_running() {
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("■ Stopping after {:.1}s…", rec.elapsed().as_secs_f64());
            let (mut project, log) = rec.finish()?;
            let project_file = project_path_for(&output);
            project.source = relative_source(&output, &project_file);
            auto::draft_captions(&mut project, &log);
            if let Some(scale) = auto_zoom {
                auto::draft_zooms(&mut project, scale);
            }
            project.save(&project_file)?;
            eprintln!(
                "Saved {} ({} cursor samples, {} clicks, {} draft captions)",
                project_file.display(),
                project.cursor.len(),
                project.clicks.len(),
                project.captions.len()
            );
            eprintln!("Next: talking-mouse edit {}", project_file.display());
            Ok(())
        }
        Cmd::New { video, output } => {
            let project_file = output.unwrap_or_else(|| project_path_for(&video));
            let project = new_project(&video, &project_file)?;
            project.save(&project_file)?;
            println!("{}", project_file.display());
            Ok(())
        }
        Cmd::Render { project, output, height, crf, preset, from, to } => {
            let p = Project::load(&project)?;
            let source = p.source_path(Some(&project));
            let opts = ExportOptions {
                output: output.clone(),
                height,
                encode: EncodeOptions { crf, preset },
                start: from,
                end: to,
            };
            let progress = Arc::new(Progress::default());
            let reporter = {
                let progress = progress.clone();
                std::thread::spawn(move || loop {
                    let (done, total) = (progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
                    if total > 0 {
                        eprint!("\rRendering {done}/{total} frames ({:.0}%)", done as f64 * 100.0 / total as f64);
                    }
                    if total > 0 && done >= total || progress.cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                })
            };
            let started = std::time::Instant::now();
            let result = render::export(&p, &source, &opts, &progress);
            progress.cancel.store(true, Ordering::Relaxed);
            let _ = reporter.join();
            eprintln!();
            result?;
            eprintln!("Wrote {} in {:.1}s", output.display(), started.elapsed().as_secs_f64());
            Ok(())
        }
        Cmd::Still { project, at, output } => {
            let p = Project::load(&project)?;
            render::still(&p, &p.source_path(Some(&project)), at, &output)
        }
    }
}

#[cfg(feature = "gui")]
fn run_editor(path: Option<PathBuf>) -> Result<()> {
    talking_mouse::editor::run(path)
}

#[cfg(not(feature = "gui"))]
fn run_editor(_: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("this build has no editor (built without the `gui` feature); use `record`, `render` or `still`")
}
