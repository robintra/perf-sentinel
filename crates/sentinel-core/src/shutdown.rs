//! Process shutdown-signal handling shared by the long-running daemon event
//! loop and the one-shot Tempo fetch loop.

/// Resolves when the process receives a shutdown signal. SIGINT (Ctrl+C) is
/// handled on every platform; SIGTERM is also handled on Unix, which is what
/// Kubernetes sends on pod termination (rolling update, scale-down), what
/// `kill` sends by default, and what systemd uses to stop a unit. Callers
/// run the same graceful cleanup for either signal. On Windows only Ctrl+C
/// applies (there is no SIGTERM).
///
/// Build this future once and `tokio::pin!` it before a `select!` loop so the
/// signal listeners are registered a single time, not re-registered on every
/// iteration.
///
/// Caveat: on Unix, registering the SIGTERM handler is process-wide and
/// permanent. Tokio never restores the OS default disposition, so once this
/// future has been awaited, SIGTERM is caught (no longer fatal by default) for
/// the rest of the process lifetime, even after the future is dropped. For a
/// one-shot caller this means the process stops terminating by default on
/// SIGTERM after the first await (SIGKILL still applies). Harmless for the
/// long-running daemon, which wants exactly that for its whole lifetime.
pub(crate) async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
            }
            Err(e) => {
                // Installing the SIGTERM handler failed: degrade to Ctrl+C
                // only rather than aborting the caller.
                tracing::warn!(
                    error = %e,
                    "failed to install SIGTERM handler; only Ctrl+C will \
                     trigger graceful shutdown"
                );
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
