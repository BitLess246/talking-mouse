//! Talking Mouse: record your screen, then pin callout captions to the cursor.

pub mod auto;
pub mod overlay;
pub mod project;
pub mod record;
pub mod render;
pub mod text;
pub mod video;

use std::path::{Path, PathBuf};

/// `demo.mkv` → `demo.tmouse.json`, next to the video.
pub fn project_path_for(video: &Path) -> PathBuf {
    video.with_extension("tmouse.json")
}

/// Stores the source path relative to the project file where possible, so
/// the pair can be moved together.
pub fn relative_source(video: &Path, project_file: &Path) -> PathBuf {
    let video_abs = std::fs::canonicalize(video).unwrap_or_else(|_| video.to_path_buf());
    let dir = project_file
        .parent()
        .map(|d| std::fs::canonicalize(if d.as_os_str().is_empty() { Path::new(".") } else { d }))
        .and_then(Result::ok);
    match dir.and_then(|d| video_abs.strip_prefix(d).ok().map(Path::to_path_buf)) {
        Some(rel) => rel,
        None => video_abs,
    }
}
