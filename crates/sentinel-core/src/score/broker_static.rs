//! Provisioned-cluster broker energy, declared rather than measured.
//!
//! A managed broker (Confluent Cloud, MSK, SQS, managed Pulsar) offers no
//! host to run an agent on, so neither Alumet nor a CPU scrape is
//! available. The operator declares the cluster instead, and the embedded
//! `SPECpower` table turns that declaration into watts.
//!
//! The model is `E(n) = n * P_max`, provisioned nodes times their power
//! ceiling. What it bounds is narrower than it reads, and it errs in both
//! directions: `docs/design/05-GREENOPS-AND-CARBON.md` has the rationale,
//! `docs/LIMITATIONS.md` the operator-facing bounds.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::score::cloud_energy::table::lookup_instance_power;

/// Milliseconds in an hour, the divisor from watt-milliseconds to kWh.
const MS_PER_HOUR: f64 = 3_600_000.0;

/// Longest gap one window may bill. Truncates rather than defers, so a
/// long idle stretch is under-counted on purpose.
const MAX_BILLABLE_MS: u64 = 3_600_000;

/// Shortest gap worth billing. Below it the time accrues into the next
/// take, keeping a sub-second tick on the borrowed fast path.
const MIN_BILLABLE_MS: u64 = 1_000;

/// Operator declaration of a provisioned broker cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticBrokerConfig {
    /// Number of provisioned broker nodes.
    pub nodes: u32,
    /// Instance type looked up in the embedded `SPECpower` table.
    pub instance_type: String,
    /// `aws`, `gcp`, `azure`, or `generic` for an on-prem default.
    /// Validation rejects anything else, an unrecognised value would
    /// silently resolve to the generic watts.
    pub provider: String,
    /// Declared region, used to convert the waste energy to gCO2.
    /// `None` reports the waste in kWh only.
    pub region: Option<String>,
}

impl StaticBrokerConfig {
    /// Watts the declared cluster draws at its provisioned ceiling.
    #[must_use]
    pub fn cluster_watts(&self) -> f64 {
        let (_idle, max_watts) = lookup_instance_power(&self.instance_type, &self.provider);
        f64::from(self.nodes) * max_watts
    }
}

/// Last-billed timestamp plus the cluster's resolved watts, so each
/// window bills only its own elapsed time. Mirrors `DbEnergyState` in
/// role, and a shed batch never advances the marker.
#[derive(Debug)]
pub struct StaticBrokerState {
    last_ms: AtomicU64,
    watts: f64,
    /// Set while this declaration covers a measurement outage, so the
    /// recovery delta can be dropped once. See `take_broker_energy`.
    billed_during_outage: AtomicBool,
}

impl StaticBrokerState {
    #[must_use]
    pub fn new(now_ms: u64, cfg: &StaticBrokerConfig) -> Self {
        Self {
            last_ms: AtomicU64::new(now_ms),
            watts: cfg.cluster_watts(),
            billed_during_outage: AtomicBool::new(false),
        }
    }

    /// Record that this declaration billed a window the measurement did
    /// not cover.
    pub fn mark_outage_billed(&self) {
        self.billed_during_outage.store(true, Ordering::SeqCst);
    }

    /// Whether this declaration has billed a stretch the measurement did not
    /// cover. Non-consuming on purpose: the marker states a fact about the
    /// timeline, not about one tick, and a tick that bills nothing must not
    /// erase it.
    pub fn outage_billed(&self) -> bool {
        self.billed_during_outage.load(Ordering::SeqCst)
    }

    /// Consume the outage marker, returning whether one was set. Only the
    /// recovery path, which acts on it, may clear it.
    pub fn clear_outage_billed(&self) -> bool {
        self.billed_during_outage.swap(false, Ordering::SeqCst)
    }

    /// Energy since the previous take, in kWh, advancing the marker.
    ///
    /// `None` when too little time has elapsed to bill, so an idle tick
    /// keeps the borrowed fast path in `build_tick_ctx`.
    pub fn take_window_kwh(&self, now_ms: u64) -> Option<f64> {
        if !self.watts.is_finite() || self.watts <= 0.0 {
            // Marker left alone on purpose: the next valid take bills this
            // stretch rather than losing it.
            return None;
        }
        let last = self.last_ms.load(Ordering::SeqCst);
        // saturating_sub covers a clock that went backwards.
        let elapsed = now_ms.saturating_sub(last);
        if elapsed < MIN_BILLABLE_MS {
            return None;
        }
        // Only a billing take advances the marker, so a lost race cannot
        // drop the elapsed time.
        if self
            .last_ms
            .compare_exchange(last, now_ms, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return None;
        }
        let kwh = self.watts * elapsed.min(MAX_BILLABLE_MS) as f64 / MS_PER_HOUR / 1000.0;
        kwh.is_finite().then_some(kwh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(nodes: u32) -> StaticBrokerConfig {
        StaticBrokerConfig {
            nodes,
            instance_type: "m5.2xlarge".to_string(),
            provider: "aws".to_string(),
            region: None,
        }
    }

    #[test]
    fn cluster_watts_scales_with_node_count() {
        let one = cfg(1).cluster_watts();
        let three = cfg(3).cluster_watts();
        assert!(one > 0.0);
        assert!((three - one * 3.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_instance_type_falls_back_to_a_provider_default() {
        let unknown = StaticBrokerConfig {
            instance_type: "not-a-real-type".to_string(),
            ..cfg(1)
        };
        assert!(unknown.cluster_watts() > 0.0);
    }

    /// A state plus the watts it resolved, so the tests can assert on
    /// energy without hard-coding the `SPECpower` row.
    fn state_at(now_ms: u64) -> (StaticBrokerState, f64) {
        let c = cfg(3);
        let watts = c.cluster_watts();
        (StaticBrokerState::new(now_ms, &c), watts)
    }

    #[test]
    fn window_energy_matches_watts_times_elapsed() {
        let (state, watts) = state_at(0);
        // One hour at `watts` is `watts / 1000` kWh.
        let kwh = state.take_window_kwh(3_600_000).expect("energy");
        assert!((kwh - watts / 1000.0).abs() < 1e-9);
    }

    #[test]
    fn a_second_take_bills_only_the_new_elapsed_time() {
        let (state, watts) = state_at(0);
        state.take_window_kwh(1_800_000).expect("first");
        let second = state.take_window_kwh(3_600_000).expect("second");
        assert!((second - watts / 2000.0).abs() < 1e-9);
    }

    #[test]
    fn no_elapsed_time_yields_nothing() {
        let (state, _) = state_at(1_000);
        assert!(state.take_window_kwh(1_000).is_none());
    }

    #[test]
    fn a_sub_second_tick_accrues_instead_of_billing() {
        let (state, watts) = state_at(0);
        // Three ticks under the threshold bill nothing and lose nothing:
        // the fourth one covers the whole elapsed stretch.
        assert!(state.take_window_kwh(300).is_none());
        assert!(state.take_window_kwh(600).is_none());
        assert!(state.take_window_kwh(900).is_none());
        let kwh = state.take_window_kwh(1_200).expect("energy");
        assert!((kwh - watts * 1_200.0 / MS_PER_HOUR / 1000.0).abs() < 1e-12);
    }

    #[test]
    fn a_long_outage_is_capped() {
        let (state, watts) = state_at(0);
        // Ten hours of downtime must bill one hour, not ten.
        let kwh = state.take_window_kwh(36_000_000).expect("energy");
        assert!((kwh - watts / 1000.0).abs() < 1e-9);
    }

    #[test]
    fn a_backwards_clock_yields_nothing() {
        let (state, _) = state_at(5_000);
        assert!(state.take_window_kwh(1_000).is_none());
    }
}
