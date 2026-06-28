//! Shared, thread-safe observability state for the outbox dispatcher loop.
//!
//! The dispatcher runs on its own thread and is observed via `/health` on the
//! server thread, so the metrics here are lock-free or mutex-guarded shared
//! state. Production code only writes from the dispatcher thread and reads
//! from the health handler; nothing here is test-only.
//!
//! See HANDOVER §4.4 for the design rationale.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

/// Observability handle shared between the dispatcher loop and the health
/// snapshot reader. Cheap to clone (one `Arc` internally is held by each
/// consumer).
#[derive(Debug)]
pub struct DispatcherMetrics {
    /// True while the dispatcher loop thread is alive. Set on entry, cleared
    /// on exit (including panic via the [`LoopGuard`] drop impl).
    running: AtomicBool,
    /// RFC3339 timestamp of the last completed poll cycle, or `None` if the
    /// loop has not ticked yet.
    last_tick_at: Mutex<Option<String>>,
    /// Sanitized category of the last dispatcher error (e.g. `timeout`,
    /// `connector_execute_failed`, `runtime_failed`), or `None` if the loop
    /// has not errored. Never the raw error string.
    last_error_category: Mutex<Option<String>>,
}

impl DispatcherMetrics {
    pub fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            last_tick_at: Mutex::new(None),
            last_error_category: Mutex::new(None),
        }
    }

    /// Mark the loop as alive. Paired with [`mark_stopped`] via [`LoopGuard`].
    pub fn mark_started(&self) {
        self.running.store(true, Ordering::SeqCst);
    }

    /// Mark the loop as no longer alive.
    pub fn mark_stopped(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Record a completed poll cycle timestamp (RFC3339).
    pub fn record_tick(&self, timestamp: String) {
        if let Ok(mut slot) = self.last_tick_at.lock() {
            *slot = Some(timestamp);
        }
    }

    /// Record a sanitized error category. Callers must pass a category string,
    /// never the raw error.
    pub fn record_error_category(&self, category: String) {
        if let Ok(mut slot) = self.last_error_category.lock() {
            *slot = Some(category);
        }
    }

    /// Whether the loop thread is currently alive.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// RFC3339 timestamp of the last poll cycle, if any.
    pub fn last_tick_at(&self) -> Option<String> {
        self.last_tick_at.lock().ok().and_then(|slot| slot.clone())
    }

    /// Last sanitized error category, if any.
    pub fn last_error_category(&self) -> Option<String> {
        self.last_error_category
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }
}

impl Default for DispatcherMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard that marks the loop running on construction and stopped on drop.
/// Using this ensures `running` is cleared even if the loop body panics.
pub struct LoopGuard<'a> {
    metrics: &'a DispatcherMetrics,
}

impl<'a> LoopGuard<'a> {
    pub fn new(metrics: &'a DispatcherMetrics) -> Self {
        metrics.mark_started();
        Self { metrics }
    }
}

impl<'a> Drop for LoopGuard<'a> {
    fn drop(&mut self) {
        self.metrics.mark_stopped();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_marks_running_then_stopped() {
        let metrics = DispatcherMetrics::new();
        assert!(!metrics.is_running());
        {
            let _guard = LoopGuard::new(&metrics);
            assert!(metrics.is_running());
        }
        assert!(!metrics.is_running());
    }

    #[test]
    fn guard_clears_running_on_panic() {
        let metrics = DispatcherMetrics::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = LoopGuard::new(&metrics);
            panic!("simulated loop panic");
        }));
        assert!(result.is_err());
        // Drop ran during unwind, so running must be cleared.
        assert!(!metrics.is_running());
    }

    #[test]
    fn records_tick_and_error_category() {
        let metrics = DispatcherMetrics::new();
        assert!(metrics.last_tick_at().is_none());
        assert!(metrics.last_error_category().is_none());
        metrics.record_tick("2026-01-01T00:00:00Z".to_string());
        metrics.record_error_category("timeout".to_string());
        assert_eq!(
            metrics.last_tick_at().as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(metrics.last_error_category().as_deref(), Some("timeout"));
    }
}
