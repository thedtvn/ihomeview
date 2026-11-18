//! In-app debug log: a ring buffer of recent lines, shown by the debug menu (hold the clock).
//!
//! The app's own lines go in through [`info`] / [`warn`] / [`error`]. Lines from other crates
//! (librespot at debug and up, everything else at warn and up) arrive through a `log` logger,
//! when the host has not installed one first. Panics are recorded too. Never log tokens.
use std::{
    collections::VecDeque,
    sync::{
        Mutex, Once,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use log::{Level, LevelFilter, Log, Metadata, Record};

const MAX_LINES: usize = 800;

static LINES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static VERSION: AtomicU64 = AtomicU64::new(0);
static CAPTURING: AtomicBool = AtomicBool::new(false);

fn push(level: &str, target: &str, message: &dyn std::fmt::Display) {
    let now = chrono::Local::now().format("%H:%M:%S%.3f");
    let line = format!("{now} {level:<5} {target}: {message}");
    let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
    while lines.len() >= MAX_LINES {
        lines.pop_front();
    }
    lines.push_back(line);
    VERSION.fetch_add(1, Ordering::Relaxed);
}

pub fn info(message: impl std::fmt::Display) {
    push("INFO", "app", &message);
}

pub fn warn(message: impl std::fmt::Display) {
    push("WARN", "app", &message);
}

pub fn error(message: impl std::fmt::Display) {
    push("ERROR", "app", &message);
}

/// Changes whenever a line is added or the log is cleared.
pub fn version() -> u64 {
    VERSION.load(Ordering::Relaxed)
}

/// All lines, oldest first, one per line.
pub fn text() -> String {
    let lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
    let mut out = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for line in lines.iter() {
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub fn line_count() -> usize {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).len()
}

pub fn clear() {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    VERSION.fetch_add(1, Ordering::Relaxed);
}

/// Whether lines from other crates (librespot) reach the log.
pub fn capturing_crates() -> bool {
    CAPTURING.load(Ordering::Relaxed)
}

struct Capture;

fn wanted(target: &str, level: Level) -> bool {
    if target.starts_with("librespot") {
        level <= Level::Debug
    } else {
        level <= Level::Warn
    }
}

impl Log for Capture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        wanted(metadata.target(), metadata.level())
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            push(record.level().as_str(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

/// Installs the logger and the panic recorder once. Safe to call more than once.
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        static CAPTURE: Capture = Capture;
        if log::set_logger(&CAPTURE).is_ok() {
            log::set_max_level(LevelFilter::Debug);
            CAPTURING.store(true, Ordering::Relaxed);
        }
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            push("ERROR", "panic", panic);
            previous(panic);
        }));
        info(format!(
            "ihomeview {} started; librespot lines {}",
            env!("CARGO_PKG_VERSION"),
            if capturing_crates() { "captured" } else { "NOT captured (host logger)" }
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_recent_lines_and_bumps_version() {
        let before = version();
        info("hello debug");
        assert!(version() > before);
        assert!(text().contains("INFO  app: hello debug"));
        for i in 0..(MAX_LINES + 5) {
            info(format!("line {i}"));
        }
        assert!(line_count() <= MAX_LINES);
        assert!(text().contains(&format!("line {}", MAX_LINES + 4)));
    }

    #[test]
    fn filters_crate_levels() {
        assert!(wanted("librespot_core::session", Level::Debug));
        assert!(!wanted("librespot_core::session", Level::Trace));
        assert!(!wanted("hyper::client", Level::Info));
        assert!(wanted("hyper::client", Level::Warn));
    }
}
