//! Provisioned-cluster broker energy, declared rather than measured.
//!
//! A managed broker (Confluent Cloud, MSK, SQS, managed Pulsar) offers no
//! host to run an agent on, so neither Alumet nor a CPU scrape is
//! available. The operator declares the cluster instead, and the embedded
//! `SPECpower` table turns that declaration into watts.
//!
//! The model is `E(n) = n * P_max`, the only published shape for
//! Kafka-class brokers. It counts **provisioned** infrastructure, not
//! consumed: a three-node cluster is immobilized whether it runs at 10 %
//! or 60 %. Two consequences to keep in mind, both documented in
//! `docs/LIMITATIONS.md`: the figure is an upper bound, and it does not
//! move when the application batches its publishes.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::score::cloud_energy::table::lookup_instance_power;

/// Milliseconds in an hour, the divisor from watt-milliseconds to kWh.
const MS_PER_HOUR: f64 = 3_600_000.0;

/// Longest gap one window may bill, 1 hour. A suspended host or a paused
/// container would otherwise bill the whole outage on the tick that
/// follows it.
const MAX_BILLABLE_MS: u64 = 3_600_000;

/// Operator declaration of a provisioned broker cluster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticBrokerConfig {
    /// Number of provisioned broker nodes.
    pub nodes: u32,
    /// Instance type looked up in the embedded `SPECpower` table.
    pub instance_type: String,
    /// `aws`, `gcp`, `azure`, or anything else for the generic default.
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

/// Last-billed timestamp, so each window bills only its own elapsed time.
///
/// Mirrors `DbEnergyState` in role: the daemon takes a per-window figure
/// once per scored batch, and a shed batch never advances the marker.
#[derive(Debug)]
pub struct StaticBrokerState {
    last_ms: AtomicU64,
}

impl StaticBrokerState {
    #[must_use]
    pub fn new(now_ms: u64) -> Self {
        Self {
            last_ms: AtomicU64::new(now_ms),
        }
    }

    /// Energy since the previous take, in kWh, advancing the marker.
    ///
    /// `None` when no time has elapsed, so an idle tick keeps the
    /// borrowed fast path in `build_tick_ctx`.
    pub fn take_window_kwh(&self, watts: f64, now_ms: u64) -> Option<f64> {
        if !watts.is_finite() || watts <= 0.0 {
            return None;
        }
        let last = self.last_ms.swap(now_ms, Ordering::SeqCst);
        // saturating_sub covers a clock that went backwards.
        let elapsed = now_ms.saturating_sub(last).min(MAX_BILLABLE_MS);
        if elapsed == 0 {
            return None;
        }
        let kwh = watts * elapsed as f64 / MS_PER_HOUR / 1000.0;
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

    #[test]
    fn window_energy_matches_watts_times_elapsed() {
        let state = StaticBrokerState::new(0);
        // 3600 W over one hour is 3.6 kWh.
        let kwh = state.take_window_kwh(3600.0, 3_600_000).expect("energy");
        assert!((kwh - 3.6).abs() < 1e-9);
    }

    #[test]
    fn a_second_take_bills_only_the_new_elapsed_time() {
        let state = StaticBrokerState::new(0);
        state.take_window_kwh(3600.0, 1_800_000).expect("first");
        let second = state.take_window_kwh(3600.0, 3_600_000).expect("second");
        assert!((second - 1.8).abs() < 1e-9);
    }

    #[test]
    fn no_elapsed_time_yields_nothing() {
        let state = StaticBrokerState::new(1_000);
        assert!(state.take_window_kwh(3600.0, 1_000).is_none());
    }

    #[test]
    fn a_long_outage_is_capped() {
        let state = StaticBrokerState::new(0);
        // Ten hours of downtime must bill one hour, not ten.
        let kwh = state.take_window_kwh(3600.0, 36_000_000).expect("energy");
        assert!((kwh - 3.6).abs() < 1e-9);
    }

    #[test]
    fn a_backwards_clock_yields_nothing() {
        let state = StaticBrokerState::new(5_000);
        assert!(state.take_window_kwh(3600.0, 1_000).is_none());
    }
}
