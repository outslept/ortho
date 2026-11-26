use std::fs::{OpenOptions, create_dir_all};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn log_path() -> PathBuf {
    let mut dir =
        std::env::var_os("LOCALAPPDATA").map_or_else(|| PathBuf::from("."), PathBuf::from);
    dir.push("ortho");
    let _ = create_dir_all(&dir);
    dir.push("ortho.log");
    dir
}

/// Appends one timestamped line to `%LOCALAPPDATA%\ortho\ortho.log`.
/// Never panics and never blocks the hotkey thread.
pub fn line(msg: &str) {
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path()) else {
        return;
    };
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let _ = writeln!(file, "{secs} {msg}");
}
