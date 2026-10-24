//! Screen recording: ffmpeg grabs the desktop while a tracker thread samples
//! the global mouse position, clicks and keystroke timing.
//!
//! Only *when* keys are pressed is stored, never which keys, so recordings of
//! password fields don't leak anything into the project file.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::project::{Click, CursorSample, Project};

#[derive(Clone, Debug)]
pub struct RecordOptions {
    /// Where to write the raw capture (`.mkv` survives being cut short).
    pub output: PathBuf,
    pub fps: u32,
    /// Capture a region `(x, y, w, h)` instead of the whole screen.
    pub region: Option<(i32, i32, u32, u32)>,
    /// Record the default microphone / audio input.
    pub audio: bool,
    /// Override ffmpeg's input (e.g. `:1.0` on X11, `2:none` on macOS).
    pub input: Option<String>,
}

/// Raw input captured alongside the video, timestamped from tracker start.
#[derive(Clone, Debug, Default)]
pub struct InputLog {
    pub cursor: Vec<CursorSample>,
    pub clicks: Vec<Click>,
    /// Mouse-up events, paired with `clicks` to detect drags.
    pub releases: Vec<Click>,
    pub keystrokes: Vec<f64>,
}

pub struct Recording {
    ffmpeg: Child,
    stdin: Option<ChildStdin>,
    stop: Arc<AtomicBool>,
    tracker: Option<JoinHandle<InputLog>>,
    /// Seconds from tracker start to the first captured frame.
    video_start: Arc<Mutex<Option<f64>>>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
    options: RecordOptions,
    started: Instant,
}

/// Pressing this chord anywhere stops a recording.
pub const STOP_HOTKEY: &str = "Ctrl+Shift+F9";
/// Input in the last moments before stopping is dropped.
const STOP_GRACE: f64 = 0.8;

impl Recording {
    pub fn start(options: RecordOptions) -> Result<Self> {
        let mut cmd = crate::video::tool("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-stats", "-y"]);
        grab_args(&mut cmd, &options)?;
        cmd.args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "16", "-pix_fmt", "yuv420p"]);
        if options.audio {
            cmd.args(["-c:a", "aac", "-b:a", "160k"]);
        }
        cmd.arg(&options.output).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());

        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let tracker = spawn_tracker(stop.clone(), started, options.region);
        let mut ffmpeg = cmd.spawn().with_context(|| crate::video::spawn_error("ffmpeg"))?;
        let stdin = ffmpeg.stdin.take();

        // `-stats` prints "frame=  N ..." lines; the first one tells us when
        // capture really began relative to the tracker clock.
        let video_start = Arc::new(Mutex::new(None));
        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let stderr = ffmpeg.stderr.take().expect("piped stderr");
        let fps = options.fps as f64;
        {
            let (video_start, tail) = (video_start.clone(), stderr_tail.clone());
            thread::spawn(move || watch_stats(stderr, started, fps, video_start, tail));
        }

        Ok(Self { ffmpeg, stdin, stop, tracker, video_start, stderr_tail, options, started })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// True once the global stop hotkey was pressed (or `stop` was called).
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Returns false if ffmpeg exited on its own (usually an error).
    pub fn is_running(&mut self) -> bool {
        matches!(self.ffmpeg.try_wait(), Ok(None))
    }

    /// Stops capture and builds a project around the recording.
    pub fn finish(mut self) -> Result<(Project, InputLog)> {
        let stop_at = self.started.elapsed().as_secs_f64();
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut stdin) = self.stdin.take() {
            // ffmpeg finalises the file cleanly when it reads 'q'.
            let _ = stdin.write_all(b"q");
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.ffmpeg.try_wait()? {
                break status;
            }
            if Instant::now() > deadline {
                let _ = self.ffmpeg.kill();
                break self.ffmpeg.wait()?;
            }
            thread::sleep(Duration::from_millis(50));
        };
        let mut log = self.tracker.take().map(|h| h.join().unwrap_or_default()).unwrap_or_default();
        // Whatever stopped the recording (the Stop button, Enter) isn't content.
        let cutoff = stop_at - STOP_GRACE;
        log.clicks.retain(|c| c.t < cutoff);
        log.releases.retain(|c| c.t < cutoff);
        log.keystrokes.retain(|&t| t < cutoff);
        if !self.options.output.exists() {
            let tail = self.stderr_tail.lock().unwrap().join("\n");
            bail!("ffmpeg did not produce a recording ({status}):\n{tail}");
        }

        let info = crate::video::probe(&self.options.output)?;
        let mut project = Project::new(self.options.output.clone(), &info);
        let start = self.video_start.lock().unwrap().unwrap_or(0.0);
        project.cursor_offset = -start;
        // Some platforms report logical points while the capture is in
        // physical pixels; infer the ratio from the screen size we saw.
        if let Some(scale) = detect_cursor_scale(&log, &info, self.options.region) {
            project.cursor_scale = scale;
        }
        project.cursor = log.cursor.clone();
        project.clicks = log.clicks.clone();
        Ok((project, log))
    }
}

fn grab_args(cmd: &mut Command, o: &RecordOptions) -> Result<()> {
    let fps = o.fps.to_string();
    if cfg!(target_os = "linux") {
        let display = match &o.input {
            Some(i) => i.clone(),
            None => match std::env::var("DISPLAY") {
                Ok(d) if !d.is_empty() => d,
                _ => bail!(
                    "no X11 DISPLAY found. On Wayland run the app under XWayland or record with \
                     your compositor's tool and use `talking-mouse new <video>`"
                ),
            },
        };
        cmd.args(["-f", "x11grab", "-framerate", &fps, "-draw_mouse", "1"]);
        match o.region {
            Some((x, y, w, h)) => {
                cmd.args(["-video_size", &format!("{}x{}", w & !1, h & !1)]);
                cmd.args(["-i", &format!("{display}+{x},{y}")]);
            }
            None => {
                cmd.args(["-i", &display]);
            }
        }
        if o.audio {
            cmd.args(["-f", "pulse", "-i", "default"]);
        }
    } else if cfg!(target_os = "macos") {
        let screen = o.input.clone().unwrap_or_else(|| "1".into());
        let audio = if o.audio { "0" } else { "none" };
        let input = if screen.contains(':') { screen } else { format!("{screen}:{audio}") };
        cmd.args(["-f", "avfoundation", "-framerate", &fps, "-capture_cursor", "1"]);
        cmd.args(["-pixel_format", "uyvy422", "-i", &input]);
        if let Some((x, y, w, h)) = o.region {
            cmd.args(["-vf", &format!("crop={}:{}:{x}:{y}", w & !1, h & !1)]);
        }
    } else if cfg!(target_os = "windows") {
        cmd.args(["-f", "gdigrab", "-framerate", &fps, "-draw_mouse", "1"]);
        if let Some((x, y, w, h)) = o.region {
            cmd.args(["-offset_x", &x.to_string(), "-offset_y", &y.to_string()]);
            cmd.args(["-video_size", &format!("{}x{}", w & !1, h & !1)]);
        }
        cmd.args(["-i", o.input.as_deref().unwrap_or("desktop")]);
    } else {
        bail!("screen recording is not supported on this platform");
    }
    // Constant frame rate output keeps video time tied to the wall clock.
    cmd.args(["-r", &fps]);
    Ok(())
}

fn watch_stats(
    stderr: impl Read,
    started: Instant,
    fps: f64,
    video_start: Arc<Mutex<Option<f64>>>,
    tail: Arc<Mutex<Vec<String>>>,
) {
    let mut reader = BufReader::new(stderr);
    let mut buf = Vec::new();
    // Stats lines end in '\r', errors in '\n'.
    while reader.read_until(b'\r', &mut buf).map(|n| n > 0).unwrap_or(false) {
        let text = String::from_utf8_lossy(&buf).to_string();
        buf.clear();
        for line in text.split(['\r', '\n']).map(str::trim).filter(|l| !l.is_empty()) {
            if let Some(rest) = line.strip_prefix("frame=") {
                let mut start = video_start.lock().unwrap();
                if start.is_none() {
                    let frames: f64 = rest.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0.0);
                    if frames > 0.0 {
                        *start = Some(started.elapsed().as_secs_f64() - frames / fps);
                    }
                }
            } else {
                let mut tail = tail.lock().unwrap();
                tail.push(line.to_string());
                if tail.len() > 20 {
                    tail.remove(0);
                }
            }
        }
    }
}

#[cfg(feature = "capture")]
fn spawn_tracker(
    stop: Arc<AtomicBool>,
    started: Instant,
    region: Option<(i32, i32, u32, u32)>,
) -> Option<JoinHandle<InputLog>> {
    use device_query::{DeviceQuery, DeviceState, Keycode};

    Some(thread::spawn(move || {
        let mut log = InputLog::default();
        let Some(device) = DeviceState::checked_new() else {
            eprintln!("warning: cannot read the global mouse position; recording without a cursor track");
            return log;
        };
        let (ox, oy) = region.map_or((0, 0), |(x, y, _, _)| (x, y));
        let mut was_down = false;
        let mut keys_down: Vec<Keycode> = Vec::new();
        let interval = Duration::from_micros(1_000_000 / 120);
        let mut last = None;
        while !stop.load(Ordering::Relaxed) {
            let tick = Instant::now();
            let t = started.elapsed().as_secs_f64();
            let mouse = device.get_mouse();
            let (x, y) = ((mouse.coords.0 - ox) as f32, (mouse.coords.1 - oy) as f32);
            if last != Some((x, y)) || log.cursor.last().is_none_or(|s| t - s.t > 0.25) {
                log.cursor.push(CursorSample { t, x, y });
                last = Some((x, y));
            }
            let down = mouse.button_pressed.iter().skip(1).any(|&b| b);
            if down && !was_down {
                log.clicks.push(Click { t, x, y });
            } else if !down && was_down {
                log.releases.push(Click { t, x, y });
            }
            was_down = down;

            let keys = device.get_keys();
            let ctrl = keys.iter().any(|k| matches!(k, Keycode::LControl | Keycode::RControl));
            let shift = keys.iter().any(|k| matches!(k, Keycode::LShift | Keycode::RShift));
            let hotkey = ctrl && shift && keys.contains(&Keycode::F9);
            if hotkey {
                stop.store(true, Ordering::Relaxed);
            }
            let is_modifier = |k: &Keycode| {
                matches!(
                    k,
                    Keycode::LControl
                        | Keycode::RControl
                        | Keycode::LShift
                        | Keycode::RShift
                        | Keycode::LAlt
                        | Keycode::RAlt
                        | Keycode::LMeta
                        | Keycode::RMeta
                        | Keycode::Command
                        | Keycode::RCommand
                )
            };
            if !hotkey && keys.iter().any(|k| !keys_down.contains(k) && !is_modifier(k)) {
                log.keystrokes.push(t);
            }
            keys_down = keys;
            if let Some(rest) = interval.checked_sub(tick.elapsed()) {
                thread::sleep(rest);
            }
        }
        log
    }))
}

#[cfg(not(feature = "capture"))]
fn spawn_tracker(_: Arc<AtomicBool>, _: Instant, _: Option<(i32, i32, u32, u32)>) -> Option<JoinHandle<InputLog>> {
    eprintln!("warning: built without the `capture` feature; recording without a cursor track");
    None
}

fn detect_cursor_scale(
    log: &InputLog,
    info: &crate::video::VideoInfo,
    region: Option<(i32, i32, u32, u32)>,
) -> Option<f64> {
    if region.is_some() || log.cursor.is_empty() {
        return None;
    }
    let max_x = log.cursor.iter().map(|c| c.x).fold(0.0f32, f32::max) as f64;
    let max_y = log.cursor.iter().map(|c| c.y).fold(0.0f32, f32::max) as f64;
    // If the pointer touched roughly half the frame's extent at most, while
    // the frame is a HiDPI size, assume 2× scaling. Otherwise leave it.
    let looks_hidpi = info.width >= 2560 && max_x <= info.width as f64 * 0.51 && max_y <= info.height as f64 * 0.51;
    (cfg!(target_os = "macos") && looks_hidpi).then_some(2.0)
}

/// Default capture path: `~/Videos/talking-mouse-<timestamp>.mkv` or cwd.
pub fn default_output(dir: Option<&Path>) -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dir = dir.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).join("Videos"))
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| PathBuf::from("."))
    });
    dir.join(format!("talking-mouse-{stamp}.mkv"))
}
