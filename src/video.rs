//! Thin wrappers around the `ffmpeg` / `ffprobe` executables. Frames move
//! through pipes as raw RGBA so no native codec bindings are needed.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};

#[derive(Clone, Debug)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    pub has_audio: bool,
}

pub fn probe(path: &Path) -> Result<VideoInfo> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_streams", "-show_format"])
        .arg(path)
        .output()
        .context("running ffprobe (is ffmpeg installed and on PATH?)")?;
    if !out.status.success() {
        bail!("ffprobe failed on {}: {}", path.display(), String::from_utf8_lossy(&out.stderr));
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let streams = json["streams"].as_array().cloned().unwrap_or_default();
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or_else(|| anyhow!("{} has no video stream", path.display()))?;
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");

    let width = video["width"].as_u64().unwrap_or(0) as u32;
    let height = video["height"].as_u64().unwrap_or(0) as u32;
    let fps = parse_rate(video["avg_frame_rate"].as_str())
        .or_else(|| parse_rate(video["r_frame_rate"].as_str()))
        .unwrap_or(30.0);
    let duration = json["format"]["duration"]
        .as_str()
        .or_else(|| video["duration"].as_str())
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    if width == 0 || height == 0 {
        bail!("could not read video size of {}", path.display());
    }
    Ok(VideoInfo { width, height, fps, duration, has_audio })
}

fn parse_rate(s: Option<&str>) -> Option<f64> {
    let (n, d) = s?.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (n > 0.0 && d > 0.0).then(|| n / d)
}

/// Streams decoded frames, resampled to a constant `fps` and scaled to
/// `width`×`height`, starting at `start` seconds.
pub struct FrameReader {
    child: Child,
    stdout: ChildStdout,
    pub width: u32,
    pub height: u32,
}

impl FrameReader {
    pub fn open(path: &Path, start: f64, fps: f64, width: u32, height: u32) -> Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin"])
            .args(["-ss", &format!("{start:.3}")])
            .arg("-i")
            .arg(path)
            .args(["-an", "-vf", &format!("fps={fps},scale={width}:{height}:flags=bicubic")])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting ffmpeg decoder (is ffmpeg installed and on PATH?)")?;
        let stdout = child.stdout.take().expect("piped stdout");
        Ok(Self { child, stdout, width, height })
    }

    /// Fills `buf` with the next frame. Returns `false` at end of stream.
    pub fn next_frame(&mut self, buf: &mut Vec<u8>) -> Result<bool> {
        buf.resize(self.width as usize * self.height as usize * 4, 0);
        let mut filled = 0;
        while filled < buf.len() {
            match self.stdout.read(&mut buf[filled..])? {
                0 => return Ok(false),
                n => filled += n,
            }
        }
        Ok(true)
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Decodes the single frame shown at `t`.
pub fn frame_at(path: &Path, t: f64, width: u32, height: u32) -> Result<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin"])
        .args(["-ss", &format!("{:.3}", t.max(0.0))])
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-an", "-vf", &format!("scale={width}:{height}:flags=bicubic")])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdin(Stdio::null())
        .output()
        .context("starting ffmpeg (is ffmpeg installed and on PATH?)")?;
    let want = width as usize * height as usize * 4;
    if out.stdout.len() < want {
        // Seeking past the last frame yields nothing; show black instead.
        return Ok(vec![0; want]);
    }
    let mut buf = out.stdout;
    buf.truncate(want);
    Ok(buf)
}

pub struct EncodeOptions {
    pub crf: u32,
    pub preset: String,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self { crf: 18, preset: "medium".into() }
    }
}

/// Encodes raw RGBA frames to H.264, muxing the audio of `audio_from`.
pub struct FrameWriter {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl FrameWriter {
    pub fn create(
        out: &Path,
        width: u32,
        height: u32,
        fps: f64,
        audio_from: Option<&Path>,
        opts: &EncodeOptions,
    ) -> Result<Self> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-y"])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
            .args(["-s", &format!("{width}x{height}"), "-r", &format!("{fps}")])
            .args(["-i", "-"]);
        if let Some(audio) = audio_from {
            cmd.arg("-i").arg(audio).args(["-map", "0:v", "-map", "1:a?", "-c:a", "aac", "-b:a", "192k"]);
        }
        cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart"])
            .args(["-preset", &opts.preset, "-crf", &opts.crf.to_string(), "-shortest"])
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn().context("starting ffmpeg encoder (is ffmpeg installed?)")?;
        let stdin = child.stdin.take();
        Ok(Self { child, stdin })
    }

    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<()> {
        self.stdin.as_mut().expect("writer already finished").write_all(rgba).context("ffmpeg encoder exited early")
    }

    pub fn finish(mut self) -> Result<()> {
        drop(self.stdin.take());
        let status = self.child.wait()?;
        if !status.success() {
            bail!("ffmpeg encoder failed ({status})");
        }
        Ok(())
    }
}

/// Picks an even-sized output resolution with the given height.
pub fn fit_height(width: u32, height: u32, out_h: u32) -> (u32, u32) {
    let w = (width as f64 * out_h as f64 / height as f64).round() as u32;
    (w & !1, out_h & !1)
}
