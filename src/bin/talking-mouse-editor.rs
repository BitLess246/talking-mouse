//! The editor as a windowed app: what desktop shortcuts and installers
//! launch. On Windows it uses the GUI subsystem so no console window opens;
//! use the `talking-mouse` binary for the command line.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // A file passed by the OS (double-click, "Open with", drag onto icon).
    let path = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    talking_mouse::editor::run(path)
}
