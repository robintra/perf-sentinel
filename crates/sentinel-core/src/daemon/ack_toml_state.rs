//! Shared `ArcSwap`-backed storage for the team-reviewed TOML acks.
//!
//! Same read-heavy / write-rare shape as
//! [`crate::score::energy_state`]: every findings query consults the map,
//! and only the reload task replaces it. `ArcSwap` keeps the read path
//! lock-free, which matters because a query holds the map for the whole
//! filtering pass.
//!
//! The map was immutable and shared behind an `Arc` until the file became a
//! mounted `ConfigMap`. An operator who edits a finding's ack expects it to
//! apply, and telling them to restart the daemon for a text file is not an
//! answer when that file is the sanctioned way to record a team decision.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::daemon::query_api::ResolvedTomlAck;

/// Signature to resolved TOML ack, swappable while the daemon serves.
#[derive(Debug, Default)]
pub struct AckTomlState {
    inner: ArcSwap<HashMap<String, ResolvedTomlAck>>,
}

impl AckTomlState {
    /// Seed the state with the map read at startup.
    #[must_use]
    pub fn new(initial: HashMap<String, ResolvedTomlAck>) -> Self {
        Self {
            inner: ArcSwap::from_pointee(initial),
        }
    }

    /// Current map. Cheap: one atomic load and an `Arc` clone, so a caller
    /// can hold it across a whole filtering pass without blocking a reload.
    #[must_use]
    pub fn load(&self) -> Arc<HashMap<String, ResolvedTomlAck>> {
        self.inner.load_full()
    }

    /// Replace the map. Readers already holding the previous one finish
    /// against it, which is what we want: a query returns a coherent view
    /// rather than one spanning two revisions of the file.
    pub fn store(&self, next: HashMap<String, ResolvedTomlAck>) {
        self.inner.store(Arc::new(next));
    }

    /// Number of acks currently held, for logs and `/api/status`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.load().len()
    }

    /// True when no ack is held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acknowledgments::Acknowledgment;

    fn ack(sig: &str) -> ResolvedTomlAck {
        ResolvedTomlAck {
            inner: Acknowledgment {
                signature: sig.to_string(),
                acknowledged_by: "team".to_string(),
                acknowledged_at: "2026-08-14".to_string(),
                reason: "test".to_string(),
                expires_at: None,
                service: None,
                source_endpoint: None,
            },
            expires_at_dt: None,
        }
    }

    #[test]
    fn a_reload_is_visible_to_the_next_reader() {
        let state = AckTomlState::new(HashMap::new());
        assert!(state.is_empty());

        let mut next = HashMap::new();
        next.insert(
            "n_plus_one_sql:svc:_ep:abc".to_string(),
            ack("n_plus_one_sql:svc:_ep:abc"),
        );
        state.store(next);

        assert_eq!(state.len(), 1);
        assert!(state.load().contains_key("n_plus_one_sql:svc:_ep:abc"));
    }

    #[test]
    fn a_reader_holding_a_snapshot_is_not_disturbed_by_a_reload() {
        // A findings query filters against one map for its whole pass. It
        // must not see half of one revision and half of the next.
        let mut initial = HashMap::new();
        initial.insert("a".to_string(), ack("a"));
        let state = AckTomlState::new(initial);

        let held = state.load();
        state.store(HashMap::new());

        assert_eq!(held.len(), 1, "the snapshot in hand stays whole");
        assert!(state.is_empty(), "while the next reader sees the new map");
    }
}
