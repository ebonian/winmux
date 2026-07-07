//! Best-effort append-only logging to `%LOCALAPPDATA%\winmux\server.log`.
//!
//! Shared by `main.rs`'s server-role bootstrap (startup/panic/exit lines)
//! and `server.rs`'s startup config loading (Task 7: `<file>:<line>: <err>`
//! entries collected while loading `.tmux.conf`/`.winmux.conf`/`--config`
//! files) — both run in the SAME headless server process but live on
//! opposite sides of the bin/lib boundary (`main.rs` cannot be `use`d from
//! `server.rs`), so this tiny module is the shared home for the one thing
//! both need. No unit tests: thin I/O glue over a real file, exercised
//! indirectly by `tests/server_proto.rs`'s config-error tests (which assert
//! on the transient attach message, not the log file itself) and manual runs.

use std::io::Write;

/// `%LOCALAPPDATA%\winmux`, or `None` if `LOCALAPPDATA` isn't set (should
/// not happen on real Windows, but this module must never panic).
fn server_log_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    let mut p = std::path::PathBuf::from(base);
    p.push("winmux");
    Some(p)
}

/// Append one line to `%LOCALAPPDATA%\winmux\server.log`, creating the
/// directory/file as needed. Best-effort: any failure (missing env var,
/// permissions, disk full) is silently swallowed — the server has no
/// console to report a logging failure to, and losing a diagnostic line is
/// far better than crashing the multiplexer over it.
pub fn log_line(msg: &str) {
    let Some(dir) = server_log_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("server.log")) {
        let _ = writeln!(f, "{msg}");
    }
}
