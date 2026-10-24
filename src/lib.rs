//! Talking Mouse: record your screen, then pin callout captions to the cursor.

pub mod auto;
#[cfg(feature = "gui")]
pub mod editor;
pub mod overlay;
pub mod project;
pub mod record;
pub mod render;
pub mod text;
pub mod video;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::project::Project;

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

/// Starts a project around an existing video file.
pub fn new_project(video: &Path, project_file: &Path) -> Result<Project> {
    if !video.exists() {
        bail!("{} does not exist", video.display());
    }
    let info = video::probe(video).with_context(|| format!("probing {}", video.display()))?;
    Ok(Project::new(relative_source(video, project_file), &info))
}
