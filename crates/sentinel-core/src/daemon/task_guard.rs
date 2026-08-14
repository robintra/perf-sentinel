//! Abort-on-drop guard for the daemon's interval-poll background tasks.

use tokio::task::JoinHandle;

/// Aborts the task on drop, so an early `?` return in the daemon startup path
/// cannot leak a detached, forever-polling task. Only for tasks that owe
/// nothing at exit: unshipped work needs a drain, like the Hub exporter's.
pub(super) struct AbortOnDrop(pub(super) JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
