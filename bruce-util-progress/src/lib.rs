//! Reporting the progress of long work, and cancelling it.
//!
//! The work takes a `&mut dyn Progress`, reports to it as it goes, e.g. "1200 of 4839 files", and checks it between
//! steps, returning the `Cancelled` error once it has been cancelled, at a point where it can stop safely. Whoever
//! called it shows the reports and keeps a `Cancel`, a clone of the one given to the work, to cancel it from elsewhere,
//! e.g. a button on another thread.
//!
//! ```
//! use bruce_util_progress::{Cancel, Cancelled, Progress, Reporter, report_count};
//!
//! fn convert(files: &[&str], progress: &mut dyn Progress) -> Result<(), Cancelled> {
//!     for (index, _file) in files.iter().enumerate() {
//!         progress.check()?;
//!         report_count(progress, index, files.len(), "files");
//!     }
//!     Ok(())
//! }
//!
//! let cancel = Cancel::new();
//! let mut shown = Vec::new();
//! {
//!     let mut progress = Reporter::new(|message: &str| shown.push(message.to_string())).with_cancel(cancel.clone());
//!     convert(&["a", "b"], &mut progress).unwrap();
//!     cancel.cancel();
//!     assert_eq!(convert(&["a", "b"], &mut progress), Err(Cancelled));
//! }
//! assert_eq!(shown, ["1 of 2 files", "2 of 2 files"]);
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Where long work reports how far it is, and asks whether it should stop.
pub trait Progress {
    /// Says how far the work is, e.g. "1200 of 4839 files".
    fn report(&mut self, message: &str);

    /// Whether the work has been asked to stop, see Cancel.
    fn is_cancelled(&self) -> bool {
        false
    }

    /// Cancelled if the work has been asked to stop, so that it can stop with `?` between steps.
    fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() { Err(Cancelled) } else { Ok(()) }
    }
}

/// Asks work to stop: a token which is cloned, one kept by whoever may cancel, one given to the work's Progress.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }

    /// Asks the work to stop, which it does at its next check.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The error of work which stopped as it was cancelled, see Progress::check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// A Progress which shows nothing and is never cancelled, for work nobody is watching.
#[derive(Debug, Clone, Copy, Default)]
pub struct Silent;

impl Progress for Silent {
    fn report(&mut self, _message: &str) {}
}

/// A Progress which gives each report to a closure, and is cancelled by a Cancel, if it has one.
pub struct Reporter<F: FnMut(&str)> {
    report: F,
    cancel: Option<Cancel>,
}

impl<F: FnMut(&str)> Reporter<F> {
    pub fn new(report: F) -> Reporter<F> {
        Reporter { report, cancel: None }
    }

    /// The same, cancelled when cancel is.
    pub fn with_cancel(mut self, cancel: Cancel) -> Reporter<F> {
        self.cancel = Some(cancel);
        self
    }
}

impl<F: FnMut(&str)> Progress for Reporter<F> {
    fn report(&mut self, message: &str) {
        (self.report)(message)
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(Cancel::is_cancelled)
    }
}

/// A Progress which puts prefix before each report, e.g. "Converting tesla, 3 of 6: ", and is cancelled when the one it
/// reports to is, for a part of some larger work.
pub struct Prefixed<'a> {
    inner: &'a mut dyn Progress,
    prefix: String,
}

impl Progress for Prefixed<'_> {
    fn report(&mut self, message: &str) {
        let message = format!("{}{message}", self.prefix);
        self.inner.report(&message)
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}

/// Reports to inner with prefix before each report, see Prefixed.
pub fn prefixed<'a>(inner: &'a mut dyn Progress, prefix: &str) -> Prefixed<'a> {
    Prefixed {
        inner,
        prefix: prefix.to_string(),
    }
}

/// About how many times report_count reports through a run of steps.
pub const REPORTS: usize = 50;

/// Reports how far through total steps the step index, from 0, is, e.g. "1200 of 4839 files", at the first, the last,
/// and about REPORTS times between, so that a few large steps say as much as thousands of small ones.
pub fn report_count(progress: &mut dyn Progress, index: usize, total: usize, what: &str) {
    if index % (total / REPORTS).max(1) == 0 || index + 1 == total {
        progress.report(&format!("{} of {total} {what}", index + 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_is_reported_about_50_times_and_at_the_first_and_last() {
        let mut shown = Vec::new();
        let mut progress = Reporter::new(|message: &str| shown.push(message.to_string()));
        for index in 0..1000 {
            report_count(&mut progress, index, 1000, "files");
        }
        assert_eq!(shown.len(), 51);
        assert_eq!(shown.first().map(String::as_str), Some("1 of 1000 files"));
        assert_eq!(shown.last().map(String::as_str), Some("1000 of 1000 files"));
    }

    #[test]
    fn a_prefixed_part_reports_with_its_prefix_and_is_cancelled_with_the_whole() {
        let cancel = Cancel::new();
        let mut shown = Vec::new();
        let mut whole = Reporter::new(|message: &str| shown.push(message.to_string())).with_cancel(cancel.clone());
        {
            let mut part = prefixed(&mut whole, "tesla: ");
            part.report("1 of 2 files");
            assert!(part.check().is_ok());
            cancel.cancel();
            assert_eq!(part.check(), Err(Cancelled));
        }
        assert_eq!(shown, ["tesla: 1 of 2 files"]);
        assert!(Silent.check().is_ok());
    }

    #[test]
    fn cancelled_is_an_error_which_can_be_told_from_others() {
        let error: Box<dyn std::error::Error> = Box::new(Cancelled);
        assert!(error.is::<Cancelled>());
        assert_eq!(error.to_string(), "cancelled");
    }
}
