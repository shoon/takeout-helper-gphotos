// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! Shared terminal progress rendering.
//!
//! Every progress bar must be registered with the same [`MultiProgress`].
//! Independent renderers compete for cursor control on Windows terminals and
//! can overwrite each other after output or a window resize. Log records and
//! direct status messages suspend this renderer while they are written, then
//! redraw the active bars at the terminal's current width.

use indicatif::{MultiProgress, ProgressBar};
use log::{Log, Metadata, Record};
use std::sync::OnceLock;

static PROGRESS: OnceLock<MultiProgress> = OnceLock::new();

/// The process-wide progress renderer.
pub fn multi_progress() -> &'static MultiProgress {
    PROGRESS.get_or_init(MultiProgress::new)
}

/// Register a bar with the process-wide renderer.
pub fn add(bar: ProgressBar) -> ProgressBar {
    multi_progress().add(bar)
}

/// Print a status line without corrupting active progress bars.
pub fn println(message: impl AsRef<str>) {
    multi_progress().suspend(|| std::println!("{}", message.as_ref()));
}

/// Print an error or warning line without corrupting active progress bars.
pub fn eprintln(message: impl AsRef<str>) {
    multi_progress().suspend(|| std::eprintln!("{}", message.as_ref()));
}

/// A logger adapter that coordinates normal log output with progress drawing.
pub struct ProgressLogger<L> {
    inner: L,
}

impl<L> ProgressLogger<L> {
    pub fn new(inner: L) -> Self {
        Self { inner }
    }
}

impl<L: Log> Log for ProgressLogger<L> {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if self.inner.enabled(record.metadata()) {
            multi_progress().suspend(|| self.inner.log(record));
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};

    struct CountingLogger(Arc<AtomicUsize>);

    impl Log for CountingLogger {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn log(&self, _record: &Record<'_>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn flush(&self) {}
    }

    #[test]
    fn shared_renderer_is_stable() {
        assert!(std::ptr::eq(multi_progress(), multi_progress()));
    }

    #[test]
    fn progress_logger_preserves_log_records() {
        let count = Arc::new(AtomicUsize::new(0));
        let logger = ProgressLogger::new(CountingLogger(count.clone()));
        let record = Record::builder()
            .level(log::Level::Warn)
            .target("progress-test")
            .args(format_args!("warning while a progress bar is active"))
            .build();

        logger.log(&record);

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
