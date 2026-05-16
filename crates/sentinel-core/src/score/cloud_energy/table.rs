//! Embedded `SPECpower` instance type lookup table.
//!
//! Maps cloud instance types to their `(idle_watts, max_watts)` envelope.
//! All entries now follow a single homogeneous methodology after the
//! 2026-04-24 refresh:
//!
//! `idle_watts = vCPU * idle_per_vcpu_coefficient`
//! `max_watts  = vCPU * max_per_vcpu_coefficient`
//!
//! Coefficients are taken per provider from [Cloud Carbon Footprint
//! coefficients] snapshot 2026-04-24 (`coefficients-{aws,gcp,azure}-use.csv`).
//! No baseboard overhead is reconstructed: the 2026-04-24 CCF no longer
//! publishes a separate baseboard column for AWS, so legacy AWS values
//! drop accordingly. For modern entries whose `SPECpower_ssj 2008`
//! direct compute (2024 Q1 - 2026 Q2) diverged by more than 5 percent
//! from the CCF per-architecture coefficient, we now align to CCF for
//! source-of-truth homogeneity. The remaining direct-compute modern
//! entries (within 5 percent of CCF or absent from the provider CSV)
//! are kept as-is and labelled `KEEP` in this file.
//!
//! Graviton 2 / 3 / 3E / 4 share the CCF EPYC 2nd Gen coefficient (no
//! published `SPECpower` submissions for these silicon variants). The
//! Cobalt 100 ARM and Ampere Altra entries that are absent from the
//! Azure / GCP CSV are kept on the `SPECpower` direct compute.
//! Sierra Forest entries (`xeon-6780e`) are 1-chip system level for
//! bare-metal users owning the full chip, not vCPU-scaled.
//!
//! Memory-optimized families (AWS r-series, GCP `n2-highmem-*`, Azure
//! `Standard_E*`) get an additive DRAM premium of `0.02 W/GB` idle and
//! `0.05 W/GB` max, applied on top of the per-vCPU CPU coefficient. The
//! coefficient is sourced from Crucial DDR4 RDIMM datasheets and the
//! Boavizta DIMM model. Memory ratio is 8 GB per vCPU for the families
//! above, giving a per-vCPU uplift of `+0.16` idle and `+0.40` max.
//! CCF 2026-04-24 does not publish a memory-class premium so this is
//! one of the two methodology departures from the CSV (the other being
//! the Turin override below). General-purpose families (`m*`) carry
//! ~4 GB/vCPU of DRAM and compute-optimized families (`c*`) carry
//! ~2 GB/vCPU; neither receives the DRAM premium under the current
//! rule, so their idle is under-counted by ~6-8 percent and ~3-4
//! percent respectively. Both stay inside the 2x uncertainty bracket.
//! EPYC 5th Gen Turin (AWS `m8a` / `c8a`) is proxied to Genoa instead
//! of importing the CCF row verbatim (the CCF Turin coefficient is
//! ~5x higher than neighbouring architectures, likely measured at chip
//! granularity by an upstream `SPECpower` submission). See
//! `docs/LIMITATIONS.md` for both rationale and uncertainty brackets.
//!
//! Full methodology, uncertainty bounds, and per-architecture caveats
//! are in `docs/LIMITATIONS.md` "Cloud `SPECpower` precision bounds".
//!
//! [Cloud Carbon Footprint coefficients]: https://github.com/cloud-carbon-footprint/ccf-coefficients/tree/b0032d928c78

use std::collections::HashMap;
use std::sync::LazyLock;

/// Vintage of the modern entries cross-checked against CCF 2026-04-24.
/// Originally `SPECpower_ssj 2008` direct compute over 2024 Q1 - 2026 Q2,
/// then aligned to CCF where the divergence exceeded 5 percent (Sapphire
/// Rapids, EPYC Genoa, Graviton). Surfaced at runtime via
/// [`super::embedded_specpower_vintage`] for inclusion in disclosure
/// reports, and via `grep` in release procedure step 2.5. Bump when the
/// underlying `SPECpower` window is extended or when CCF publishes a
/// new coefficient set.
pub(crate) const SPECPOWER_VINTAGE: &str = "2026-04-24 (CCF aligned)";

/// Vintage of the AWS / GCP / Azure entries derived from
/// `ccf-coefficients` per-architecture coefficients. The original
/// snapshot was 2023-05-01 (archived `cloud-carbon-coefficients` repo);
/// the active successor `ccf-coefficients` published a refresh on
/// 2026-04-24 that we adopted with a homogeneous `vCPU * coefficient`
/// methodology across all providers and families. The AWS baseboard
/// overhead column from the 2023-05-01 snapshot is no longer published
/// in 2026-04-24, so it is dropped uniformly.
#[allow(dead_code)]
pub(crate) const CCF_LEGACY_VINTAGE: &str = "2026-04-24";

/// Memory-optimized DRAM premium, applied additively on top of the
/// per-vCPU CPU coefficient for r-series / highmem / `Standard_E*`
/// families. `0.02 W/GB` idle, `0.05 W/GB` max, sourced from Crucial
/// DDR4 RDIMM datasheets and the Boavizta DIMM model. The 8 GB / vCPU
/// memory ratio of those families gives a per-vCPU uplift of
/// `+0.16` idle / `+0.40` max, embedded inline below.
#[allow(dead_code)]
pub(crate) const DRAM_PREMIUM_W_PER_GB_IDLE: f64 = 0.02;
#[allow(dead_code)]
pub(crate) const DRAM_PREMIUM_W_PER_GB_MAX: f64 = 0.05;

/// `(idle_watts, max_watts)` per instance type.
///
/// Idle watts represent power at near-zero CPU load. Max watts represent
/// power at 100% CPU utilization. Values are `vCPU * per_vCPU_coefficient`
/// from CCF 2026-04-24, with no separate baseboard term (CCF stopped
/// publishing one for AWS in this snapshot). See the module-level docs
/// for the modern entries kept on `SPECpower` direct compute.
static INSTANCE_POWER: LazyLock<HashMap<&'static str, (f64, f64)>> = LazyLock::new(|| {
    let entries: &[(&str, f64, f64)] = &[
        // ================================================================
        // AWS instances (vCPU * per_vCPU_coefficient from CCF 2026-04-24
        // coefficients-aws-use.csv; baseboard overhead column no longer
        // published, dropped uniformly)
        // ================================================================

        // --- t3 (Nitro, Cascade Lake, burstable) ---
        // CCF Cascade Lake: 0.690 idle / 4.063 max W/vCPU. Burst credit
        // is not modeled by CCF; all sizes with 2 vCPU report identically.
        ("t3.nano", 1.4, 8.1),
        ("t3.micro", 1.4, 8.1),
        ("t3.small", 1.4, 8.1),
        ("t3.medium", 1.4, 8.1),
        ("t3.large", 1.4, 8.1),
        ("t3.xlarge", 2.8, 16.3),
        ("t3.2xlarge", 5.5, 32.5),
        // --- t3a (Nitro, EPYC 1st Gen Naples, burstable) ---
        // CCF EPYC 1st Gen: 0.847 idle / 2.604 max W/vCPU.
        ("t3a.nano", 1.7, 5.2),
        ("t3a.micro", 1.7, 5.2),
        ("t3a.small", 1.7, 5.2),
        ("t3a.medium", 1.7, 5.2),
        ("t3a.large", 1.7, 5.2),
        ("t3a.xlarge", 3.4, 10.4),
        ("t3a.2xlarge", 6.8, 20.8),
        // --- m5 (Cascade Lake, general purpose) ---
        ("m5.large", 1.4, 8.1),
        ("m5.xlarge", 2.8, 16.3),
        ("m5.2xlarge", 5.5, 32.5),
        ("m5.4xlarge", 11.0, 65.0),
        ("m5.8xlarge", 22.1, 130.0),
        ("m5.12xlarge", 33.1, 195.0),
        ("m5.16xlarge", 44.2, 260.0),
        ("m5.24xlarge", 66.3, 390.1),
        // --- m5a (EPYC 1st Gen Naples, general purpose) ---
        ("m5a.large", 1.7, 5.2),
        ("m5a.xlarge", 3.4, 10.4),
        ("m5a.2xlarge", 6.8, 20.8),
        ("m5a.4xlarge", 13.5, 41.7),
        ("m5a.8xlarge", 27.1, 83.3),
        ("m5a.12xlarge", 40.6, 125.0),
        ("m5a.16xlarge", 54.2, 166.7),
        ("m5a.24xlarge", 81.3, 250.0),
        // --- c5 (Cascade Lake, compute-optimized) ---
        // CCF does not differentiate compute vs general purpose: same
        // per-vCPU coefficient as m5. The 9xlarge (36 vCPU) and 18xlarge
        // (72 vCPU) sizes follow AWS-published vCPU counts.
        ("c5.large", 1.4, 8.1),
        ("c5.xlarge", 2.8, 16.3),
        ("c5.2xlarge", 5.5, 32.5),
        ("c5.4xlarge", 11.0, 65.0),
        ("c5.9xlarge", 24.9, 146.3),
        ("c5.12xlarge", 33.1, 195.0),
        ("c5.18xlarge", 49.7, 292.5),
        ("c5.24xlarge", 66.3, 390.1),
        // --- c5a (EPYC 2nd Gen Rome, compute-optimized) ---
        // CCF EPYC 2nd Gen: 0.474 idle / 1.693 max W/vCPU.
        ("c5a.large", 0.9, 3.4),
        ("c5a.xlarge", 1.9, 6.8),
        ("c5a.2xlarge", 3.8, 13.5),
        ("c5a.4xlarge", 7.6, 27.1),
        ("c5a.8xlarge", 15.2, 54.2),
        ("c5a.12xlarge", 22.8, 81.3),
        ("c5a.16xlarge", 30.4, 108.3),
        ("c5a.24xlarge", 45.5, 162.5),
        // --- r5 (Cascade Lake, memory-optimized) ---
        // CCF Cascade Lake (0.690/4.063) + DRAM premium (+0.16/+0.40 per
        // vCPU at 8 GB/vCPU). Final per-vCPU coefficient 0.850/4.463.
        ("r5.large", 1.7, 8.9),
        ("r5.xlarge", 3.4, 17.9),
        ("r5.2xlarge", 6.8, 35.7),
        ("r5.4xlarge", 13.6, 71.4),
        ("r5.8xlarge", 27.2, 142.8),
        ("r5.12xlarge", 40.8, 214.2),
        ("r5.16xlarge", 54.4, 285.6),
        ("r5.24xlarge", 81.6, 428.4),
        // --- r5a (EPYC 1st Gen Naples, memory-optimized) ---
        // CCF EPYC 1st Gen (0.847/2.604) + DRAM premium. Final 1.007/3.004.
        ("r5a.large", 2.0, 6.0),
        ("r5a.xlarge", 4.0, 12.0),
        ("r5a.2xlarge", 8.1, 24.0),
        ("r5a.4xlarge", 16.1, 48.1),
        ("r5a.8xlarge", 32.2, 96.1),
        ("r5a.12xlarge", 48.3, 144.2),
        ("r5a.16xlarge", 64.4, 192.3),
        ("r5a.24xlarge", 96.7, 288.4),
        // --- m6i (Ice Lake, general purpose) ---
        // CCF Ice Lake: 0.767 idle / 3.758 max W/vCPU.
        ("m6i.large", 1.5, 7.5),
        ("m6i.xlarge", 3.1, 15.0),
        ("m6i.2xlarge", 6.1, 30.1),
        ("m6i.4xlarge", 12.3, 60.1),
        ("m6i.8xlarge", 24.5, 120.3),
        ("m6i.12xlarge", 36.8, 180.4),
        ("m6i.16xlarge", 49.1, 240.5),
        ("m6i.24xlarge", 73.6, 360.8),
        ("m6i.32xlarge", 98.2, 481.0),
        // --- c6i (Ice Lake, compute-optimized) ---
        ("c6i.large", 1.5, 7.5),
        ("c6i.xlarge", 3.1, 15.0),
        ("c6i.2xlarge", 6.1, 30.1),
        ("c6i.4xlarge", 12.3, 60.1),
        ("c6i.8xlarge", 24.5, 120.3),
        ("c6i.12xlarge", 36.8, 180.4),
        ("c6i.16xlarge", 49.1, 240.5),
        ("c6i.24xlarge", 73.6, 360.8),
        ("c6i.32xlarge", 98.2, 481.0),
        // --- r6i (Ice Lake, memory-optimized) ---
        // CCF Ice Lake (0.767/3.758) + DRAM premium. Final 0.927/4.158.
        ("r6i.large", 1.9, 8.3),
        ("r6i.xlarge", 3.7, 16.6),
        ("r6i.2xlarge", 7.4, 33.3),
        ("r6i.4xlarge", 14.8, 66.5),
        ("r6i.8xlarge", 29.7, 133.1),
        ("r6i.12xlarge", 44.5, 199.6),
        ("r6i.16xlarge", 59.3, 266.1),
        ("r6i.24xlarge", 89.0, 399.2),
        // --- m7i (Sapphire Rapids, Xeon Platinum 8488C) ---
        // CCF Sapphire Rapids: 1.036 idle / 4.160 max W/vCPU (REFRESH:
        // SPECpower direct compute diverged ~48% idle / ~19% max).
        ("m7i.large", 2.1, 8.3),
        ("m7i.xlarge", 4.1, 16.6),
        ("m7i.2xlarge", 8.3, 33.3),
        ("m7i.4xlarge", 16.6, 66.6),
        ("m7i.8xlarge", 33.2, 133.1),
        ("m7i.16xlarge", 66.3, 266.3),
        // --- c7i (Sapphire Rapids, compute-optimized) ---
        ("c7i.large", 2.1, 8.3),
        ("c7i.xlarge", 4.1, 16.6),
        ("c7i.2xlarge", 8.3, 33.3),
        ("c7i.4xlarge", 16.6, 66.6),
        ("c7i.8xlarge", 33.2, 133.1),
        ("c7i.16xlarge", 66.3, 266.3),
        // --- r7i (Sapphire Rapids, memory-optimized) ---
        // CCF Sapphire Rapids (1.036/4.160) + DRAM premium. Final 1.196/4.560.
        ("r7i.large", 2.4, 9.1),
        ("r7i.xlarge", 4.8, 18.2),
        ("r7i.2xlarge", 9.6, 36.5),
        ("r7i.4xlarge", 19.1, 73.0),
        ("r7i.8xlarge", 38.3, 145.9),
        ("r7i.16xlarge", 76.5, 291.8),
        // --- m7a (AMD Genoa, EPYC 9R14) ---
        // CCF EPYC 4th Gen: 0.739 idle / 2.282 max W/vCPU (REFRESH:
        // SPECpower direct compute diverged ~85% idle / ~11% max).
        ("m7a.large", 1.5, 4.6),
        ("m7a.xlarge", 3.0, 9.1),
        ("m7a.2xlarge", 5.9, 18.3),
        ("m7a.4xlarge", 11.8, 36.5),
        ("m7a.8xlarge", 23.7, 73.0),
        ("m7a.16xlarge", 47.3, 146.1),
        // --- c7a (Genoa, compute-optimized) ---
        ("c7a.large", 1.5, 4.6),
        ("c7a.xlarge", 3.0, 9.1),
        ("c7a.2xlarge", 5.9, 18.3),
        ("c7a.4xlarge", 11.8, 36.5),
        ("c7a.8xlarge", 23.7, 73.0),
        ("c7a.16xlarge", 47.3, 146.1),
        // --- r7a (Genoa, memory-optimized) ---
        // CCF EPYC 4th Gen Genoa (0.739/2.282) + DRAM premium. Final 0.899/2.682.
        ("r7a.large", 1.8, 5.4),
        ("r7a.xlarge", 3.6, 10.7),
        ("r7a.2xlarge", 7.2, 21.5),
        ("r7a.4xlarge", 14.4, 42.9),
        ("r7a.8xlarge", 28.8, 85.8),
        ("r7a.16xlarge", 57.5, 171.6),
        // --- m6a (AMD Milan, EPYC 7R13) ---
        // CCF EPYC 3rd Gen: 0.456 idle / 1.957 max W/vCPU (KEEP: existing
        // SPECpower direct compute is within 5% of CCF on idle and max).
        ("m6a.large", 0.9, 4.0),
        ("m6a.xlarge", 1.8, 8.1),
        ("m6a.2xlarge", 3.6, 16.2),
        ("m6a.4xlarge", 7.1, 32.3),
        ("m6a.8xlarge", 14.2, 64.6),
        ("m6a.16xlarge", 28.5, 129.2),
        // --- c6a (Milan, compute-optimized) ---
        ("c6a.large", 0.9, 4.0),
        ("c6a.xlarge", 1.8, 8.1),
        ("c6a.2xlarge", 3.6, 16.2),
        ("c6a.4xlarge", 7.1, 32.3),
        ("c6a.8xlarge", 14.2, 64.6),
        ("c6a.16xlarge", 28.5, 129.2),
        // --- m7g (Graviton 3, Neoverse V1) ---
        // CCF Graviton 3 = EPYC 2nd Gen proxy: 0.474 idle / 1.693 max
        // W/vCPU. CCF has no measured SPECpower for Graviton silicon
        // (REFRESH from earlier Altra-floor heuristic).
        ("m7g.large", 0.9, 3.4),
        ("m7g.xlarge", 1.9, 6.8),
        ("m7g.2xlarge", 3.8, 13.5),
        ("m7g.4xlarge", 7.6, 27.1),
        ("m7g.8xlarge", 15.2, 54.2),
        ("m7g.16xlarge", 30.4, 108.3),
        // --- c7g (Graviton 3, compute-optimized) ---
        ("c7g.large", 0.9, 3.4),
        ("c7g.xlarge", 1.9, 6.8),
        ("c7g.2xlarge", 3.8, 13.5),
        ("c7g.4xlarge", 7.6, 27.1),
        ("c7g.8xlarge", 15.2, 54.2),
        ("c7g.16xlarge", 30.4, 108.3),
        // --- m8g (Graviton 4, Neoverse V2) ---
        // CCF Graviton 4 = EPYC 2nd Gen proxy: 0.474 idle / 1.693 max.
        // AWS publicly claims Graviton 4 is more efficient than Graviton
        // 3 but no SPECpower data exists, so CCF reuses the same value.
        ("m8g.large", 0.9, 3.4),
        ("m8g.xlarge", 1.9, 6.8),
        ("m8g.2xlarge", 3.8, 13.5),
        ("m8g.4xlarge", 7.6, 27.1),
        ("m8g.8xlarge", 15.2, 54.2),
        ("m8g.16xlarge", 30.4, 108.3),
        // --- c8g (Graviton 4, compute-optimized) ---
        ("c8g.large", 0.9, 3.4),
        ("c8g.xlarge", 1.9, 6.8),
        ("c8g.2xlarge", 3.8, 13.5),
        ("c8g.4xlarge", 7.6, 27.1),
        ("c8g.8xlarge", 15.2, 54.2),
        ("c8g.16xlarge", 30.4, 108.3),
        // --- m8a (AMD Turin, EPYC 5th Gen, general purpose) ---
        // CCF EPYC 5th Gen 2026-04-24 publishes 3.682 idle / 8.961 max
        // W/vCPU, ~5x higher than neighbouring architectures (Genoa
        // 0.739/2.282). Most likely a tiny upstream SPECpower sample
        // measured at chip rather than thread granularity. We proxy
        // Turin to Genoa pending an upstream CCF correction: this
        // preserves directional waste-signal credibility for m8a/c8a
        // customers and avoids a silent 4x carbon inflation. Tracked
        // in docs/LIMITATIONS.md for re-evaluation on next CCF refresh.
        ("m8a.large", 1.5, 4.6),
        ("m8a.xlarge", 3.0, 9.1),
        ("m8a.2xlarge", 5.9, 18.3),
        ("m8a.4xlarge", 11.8, 36.5),
        ("m8a.8xlarge", 23.7, 73.0),
        ("m8a.16xlarge", 47.3, 146.1),
        // --- c8a (Turin, compute-optimized, Genoa proxy) ---
        ("c8a.large", 1.5, 4.6),
        ("c8a.xlarge", 3.0, 9.1),
        ("c8a.2xlarge", 5.9, 18.3),
        ("c8a.4xlarge", 11.8, 36.5),
        ("c8a.8xlarge", 23.7, 73.0),
        ("c8a.16xlarge", 47.3, 146.1),
        // --- m8i (Intel Emerald Rapids, general purpose) ---
        // CCF Emerald Rapids: 0.814 idle / 4.482 max W/vCPU.
        ("m8i.large", 1.6, 9.0),
        ("m8i.xlarge", 3.3, 17.9),
        ("m8i.2xlarge", 6.5, 35.9),
        ("m8i.4xlarge", 13.0, 71.7),
        ("m8i.8xlarge", 26.0, 143.4),
        ("m8i.16xlarge", 52.1, 286.9),
        // --- c8i (Emerald Rapids, compute-optimized) ---
        ("c8i.large", 1.6, 9.0),
        ("c8i.xlarge", 3.3, 17.9),
        ("c8i.2xlarge", 6.5, 35.9),
        ("c8i.4xlarge", 13.0, 71.7),
        ("c8i.8xlarge", 26.0, 143.4),
        ("c8i.16xlarge", 52.1, 286.9),
        // ================================================================
        // GCP instances (vCPU * per_vCPU_coefficient from CCF 2026-04-24
        // coefficients-gcp-use.csv)
        // ================================================================

        // --- n2-standard (Cascade Lake, general purpose) ---
        // CCF GCP Cascade Lake: 0.690 idle / 3.755 max W/vCPU.
        ("n2-standard-2", 1.4, 7.5),
        ("n2-standard-4", 2.8, 15.0),
        ("n2-standard-8", 5.5, 30.0),
        ("n2-standard-16", 11.0, 60.1),
        ("n2-standard-32", 22.1, 120.2),
        ("n2-standard-48", 33.1, 180.2),
        ("n2-standard-64", 44.2, 240.3),
        ("n2-standard-80", 55.2, 300.4),
        ("n2-standard-96", 66.3, 360.5),
        ("n2-standard-128", 88.4, 480.6),
        // --- n2-highcpu (Cascade Lake, compute-optimized) ---
        ("n2-highcpu-2", 1.4, 7.5),
        ("n2-highcpu-4", 2.8, 15.0),
        ("n2-highcpu-8", 5.5, 30.0),
        ("n2-highcpu-16", 11.0, 60.1),
        ("n2-highcpu-32", 22.1, 120.2),
        ("n2-highcpu-48", 33.1, 180.2),
        ("n2-highcpu-64", 44.2, 240.3),
        ("n2-highcpu-80", 55.2, 300.4),
        ("n2-highcpu-96", 66.3, 360.5),
        // --- n2-highmem (Cascade Lake, memory-optimized) ---
        // CCF GCP Cascade Lake (0.690/3.755) + DRAM premium (+0.16/+0.40
        // per vCPU at 8 GB/vCPU). Final per-vCPU coefficient 0.850/4.155.
        ("n2-highmem-2", 1.7, 8.3),
        ("n2-highmem-4", 3.4, 16.6),
        ("n2-highmem-8", 6.8, 33.2),
        ("n2-highmem-16", 13.6, 66.5),
        ("n2-highmem-32", 27.2, 133.0),
        ("n2-highmem-48", 40.8, 199.4),
        ("n2-highmem-64", 54.4, 265.9),
        ("n2-highmem-80", 68.0, 332.4),
        ("n2-highmem-96", 81.6, 398.9),
        ("n2-highmem-128", 108.8, 531.8),
        // --- e2-standard (EPYC 2nd Gen / Skylake mix, general purpose) ---
        // CCF GCP EPYC 2nd Gen: 0.474 idle / 1.575 max W/vCPU.
        ("e2-standard-2", 0.9, 3.2),
        ("e2-standard-4", 1.9, 6.3),
        ("e2-standard-8", 3.8, 12.6),
        ("e2-standard-16", 7.6, 25.2),
        ("e2-standard-32", 15.2, 50.4),
        // --- c2-standard (Cascade Lake, compute-optimized) ---
        ("c2-standard-4", 2.8, 15.0),
        ("c2-standard-8", 5.5, 30.0),
        ("c2-standard-16", 11.0, 60.1),
        ("c2-standard-30", 20.7, 112.6),
        ("c2-standard-60", 41.4, 225.3),
        // --- c3 (Sapphire Rapids, general purpose) ---
        // CCF GCP Sapphire Rapids: 1.036 idle / 4.062 max W/vCPU
        // (REFRESH: SPECpower direct compute diverged ~48% idle / ~16% max).
        ("c3-standard-4", 4.1, 16.2),
        ("c3-standard-8", 8.3, 32.5),
        ("c3-standard-22", 22.8, 89.4),
        ("c3-standard-44", 45.6, 178.7),
        ("c3-standard-88", 91.2, 357.5),
        ("c3-standard-176", 182.4, 714.9),
        // --- c3d (AMD Genoa, EPYC 9004) ---
        // CCF GCP EPYC 4th Gen: 0.739 idle / 2.196 max W/vCPU
        // (REFRESH: SPECpower direct compute diverged ~85% idle / ~7% max).
        ("c3d-standard-4", 3.0, 8.8),
        ("c3d-standard-8", 5.9, 17.6),
        ("c3d-standard-16", 11.8, 35.1),
        ("c3d-standard-30", 22.2, 65.9),
        ("c3d-standard-60", 44.3, 131.8),
        ("c3d-standard-180", 133.0, 395.3),
        // --- c4 (Emerald Rapids, Xeon Platinum 8592+) ---
        // CCF GCP Emerald Rapids: 0.814 idle / 4.382 max W/vCPU
        // (REFRESH: SPECpower direct compute diverged ~48% idle / ~37% max).
        ("c4-standard-2", 1.6, 8.8),
        ("c4-standard-4", 3.3, 17.5),
        ("c4-standard-8", 6.5, 35.1),
        ("c4-standard-16", 13.0, 70.1),
        ("c4-standard-32", 26.0, 140.2),
        ("c4-standard-96", 78.1, 420.6),
        // --- c4d (AMD Turin, EPYC 9005 Zen 5) ---
        // CCF GCP CSV 2026-04-24 does not publish EPYC 5th Gen Turin
        // coefficients (Google's deployment was not yet mapped at the
        // CCF snapshot). Kept on the SPECpower direct compute (n=9
        // EPYC 9655/9755, 0.32/1.91 W/vCPU) from 2024 Q4 - 2026 Q2.
        ("c4d-standard-2", 0.6, 3.8),
        ("c4d-standard-4", 1.3, 7.6),
        ("c4d-standard-8", 2.6, 15.3),
        ("c4d-standard-16", 5.1, 30.6),
        ("c4d-standard-32", 10.2, 61.1),
        ("c4d-standard-96", 30.7, 183.4),
        // --- n2d (Genoa-era newer, EPYC 9004) ---
        // CCF GCP EPYC 4th Gen: 0.739 idle / 2.196 max W/vCPU
        // (REFRESH: SPECpower direct compute diverged ~85% idle / ~7% max).
        ("n2d-standard-2", 1.5, 4.4),
        ("n2d-standard-4", 3.0, 8.8),
        ("n2d-standard-8", 5.9, 17.6),
        ("n2d-standard-16", 11.8, 35.1),
        ("n2d-standard-32", 23.7, 70.3),
        ("n2d-standard-64", 47.3, 140.5),
        // --- t2a (Ampere Altra, Neoverse N1) ---
        // CCF GCP CSV 2026-04-24 has no Ampere Altra entry. Kept on the
        // SPECpower direct compute (n=1 Altra Q80-30, 0.67/1.75 W/vCPU).
        ("t2a-standard-1", 0.7, 1.8),
        ("t2a-standard-2", 1.3, 3.5),
        ("t2a-standard-4", 2.7, 7.0),
        ("t2a-standard-8", 5.4, 14.0),
        ("t2a-standard-16", 10.7, 28.0),
        ("t2a-standard-32", 21.4, 56.0),
        // --- c4a (Google Axion, Neoverse V2 ARM) ---
        // No native ARM entry in the GCP CSV. Proxied to AWS Graviton 4
        // (Neoverse V2 silicon family), itself mapped by CCF to EPYC 2nd
        // Gen as a conservative placeholder: 0.474 idle / 1.693 max W/vCPU.
        ("c4a-standard-1", 0.5, 1.7),
        ("c4a-standard-2", 0.9, 3.4),
        ("c4a-standard-4", 1.9, 6.8),
        ("c4a-standard-8", 3.8, 13.5),
        ("c4a-standard-16", 7.6, 27.1),
        ("c4a-standard-32", 15.2, 54.2),
        ("c4a-standard-48", 22.8, 81.3),
        ("c4a-standard-72", 34.1, 121.9),
        // ================================================================
        // Azure instances (vCPU * per_vCPU_coefficient from CCF 2026-04-24
        // coefficients-azure-use.csv). CCF Azure CSV 2026-04-24 does not
        // publish Sapphire Rapids, Emerald Rapids, or EPYC 4th Gen Genoa
        // coefficients for Azure deployments, so the v6 families remain
        // on their SPECpower direct compute (2024 Q1 - 2026 Q2) and no
        // new Azure architecture is added in this refresh.
        // ================================================================

        // --- Standard_D v3 (Broadwell / Skylake) ---
        // CCF Azure Skylake: 0.645 idle / 4.193 max W/vCPU.
        ("Standard_D2s_v3", 1.3, 8.4),
        ("Standard_D4s_v3", 2.6, 16.8),
        ("Standard_D8s_v3", 5.2, 33.5),
        ("Standard_D16s_v3", 10.3, 67.1),
        ("Standard_D32s_v3", 20.6, 134.2),
        ("Standard_D48s_v3", 31.0, 201.3),
        ("Standard_D64s_v3", 41.3, 268.4),
        // --- Standard_D v4 (Cascade Lake) ---
        // CCF Azure Cascade Lake: 0.639 idle / 3.967 max W/vCPU.
        ("Standard_D2s_v4", 1.3, 7.9),
        ("Standard_D4s_v4", 2.6, 15.9),
        ("Standard_D8s_v4", 5.1, 31.7),
        ("Standard_D16s_v4", 10.2, 63.5),
        ("Standard_D32s_v4", 20.4, 126.9),
        ("Standard_D48s_v4", 30.7, 190.4),
        ("Standard_D64s_v4", 40.9, 253.9),
        // --- Standard_D v5 (Cascade Lake / Ice Lake) ---
        ("Standard_D2s_v5", 1.3, 7.9),
        ("Standard_D4s_v5", 2.6, 15.9),
        ("Standard_D8s_v5", 5.1, 31.7),
        ("Standard_D16s_v5", 10.2, 63.5),
        ("Standard_D32s_v5", 20.4, 126.9),
        ("Standard_D48s_v5", 30.7, 190.4),
        ("Standard_D64s_v5", 40.9, 253.9),
        ("Standard_D96s_v5", 61.3, 380.8),
        // --- Standard_Das v5 (EPYC 3rd Gen) ---
        // CCF Azure EPYC 3rd Gen: 0.445 idle / 2.019 max W/vCPU.
        ("Standard_D2as_v5", 0.9, 4.0),
        ("Standard_D4as_v5", 1.8, 8.1),
        ("Standard_D8as_v5", 3.6, 16.2),
        ("Standard_D16as_v5", 7.1, 32.3),
        ("Standard_D32as_v5", 14.2, 64.6),
        ("Standard_D48as_v5", 21.4, 96.9),
        ("Standard_D64as_v5", 28.5, 129.2),
        ("Standard_D96as_v5", 42.7, 193.8),
        // --- Standard_E v3 (Skylake, memory-optimized) ---
        // CCF Azure Skylake (0.645/4.193) + DRAM premium (+0.16/+0.40
        // per vCPU at 8 GB/vCPU). Final 0.805/4.593.
        ("Standard_E2s_v3", 1.6, 9.2),
        ("Standard_E4s_v3", 3.2, 18.4),
        ("Standard_E8s_v3", 6.4, 36.7),
        ("Standard_E16s_v3", 12.9, 73.5),
        ("Standard_E32s_v3", 25.8, 147.0),
        ("Standard_E48s_v3", 38.6, 220.5),
        ("Standard_E64s_v3", 51.5, 294.0),
        // --- Standard_E v4 (Cascade Lake, memory-optimized) ---
        // CCF Azure Cascade Lake (0.639/3.967) + DRAM premium. Final 0.799/4.367.
        ("Standard_E2s_v4", 1.6, 8.7),
        ("Standard_E4s_v4", 3.2, 17.5),
        ("Standard_E8s_v4", 6.4, 34.9),
        ("Standard_E16s_v4", 12.8, 69.9),
        ("Standard_E32s_v4", 25.6, 139.7),
        ("Standard_E48s_v4", 38.4, 209.6),
        ("Standard_E64s_v4", 51.1, 279.5),
        // --- Standard_E v5 (Cascade Lake / Ice Lake, memory-optimized) ---
        // Same base coefficient as v4 (Cascade Lake) + DRAM premium.
        ("Standard_E2s_v5", 1.6, 8.7),
        ("Standard_E4s_v5", 3.2, 17.5),
        ("Standard_E8s_v5", 6.4, 34.9),
        ("Standard_E16s_v5", 12.8, 69.9),
        ("Standard_E32s_v5", 25.6, 139.7),
        ("Standard_E48s_v5", 38.4, 209.6),
        ("Standard_E64s_v5", 51.1, 279.5),
        ("Standard_E96s_v5", 76.7, 419.2),
        // --- Standard_F v2 (Cascade Lake, compute-optimized) ---
        ("Standard_F2s_v2", 1.3, 7.9),
        ("Standard_F4s_v2", 2.6, 15.9),
        ("Standard_F8s_v2", 5.1, 31.7),
        ("Standard_F16s_v2", 10.2, 63.5),
        ("Standard_F32s_v2", 20.4, 126.9),
        ("Standard_F48s_v2", 30.7, 190.4),
        ("Standard_F64s_v2", 40.9, 253.9),
        ("Standard_F72s_v2", 46.0, 285.6),
        // --- Standard_D v6 (Emerald Rapids, Xeon Platinum 8573C) ---
        // Not in CCF Azure CSV 2026-04-24. Kept on SPECpower direct
        // compute (2024 Q1-Q2, n=18 Platinum 8592+/8581V, 0.55/3.20 W/vCPU).
        ("Standard_D2s_v6", 1.1, 6.4),
        ("Standard_D4s_v6", 2.2, 12.8),
        ("Standard_D8s_v6", 4.4, 25.6),
        ("Standard_D16s_v6", 8.8, 51.2),
        ("Standard_D32s_v6", 17.6, 102.4),
        ("Standard_D64s_v6", 35.2, 204.8),
        ("Standard_D96s_v6", 52.8, 307.2),
        // --- Standard_Dads v6 (AMD Genoa, EPYC 9004) ---
        // Not in CCF Azure CSV 2026-04-24. Kept on SPECpower direct
        // compute, 0.40/2.05 W/vCPU per Genoa coefficient.
        ("Standard_D2ads_v6", 0.8, 4.1),
        ("Standard_D4ads_v6", 1.6, 8.2),
        ("Standard_D8ads_v6", 3.2, 16.4),
        ("Standard_D16ads_v6", 6.4, 32.8),
        ("Standard_D32ads_v6", 12.8, 65.6),
        ("Standard_D64ads_v6", 25.6, 131.2),
        ("Standard_D96ads_v6", 38.4, 196.8),
        // --- Standard_Dps v6 (Microsoft Cobalt 100, Neoverse N2 ARM) ---
        // N2 sits between Altra N1 (0.67/1.75) and Graviton 3 V1 (0.53/2.63).
        // Midpoint blend = 0.60/2.20 W/vCPU pending direct Cobalt SPECpower data.
        ("Standard_D2ps_v6", 1.2, 4.4),
        ("Standard_D4ps_v6", 2.4, 8.8),
        ("Standard_D8ps_v6", 4.8, 17.6),
        ("Standard_D16ps_v6", 9.6, 35.2),
        ("Standard_D32ps_v6", 19.2, 70.4),
        ("Standard_D64ps_v6", 38.4, 140.8),
        ("Standard_D96ps_v6", 57.6, 211.2),
        // --- Standard_E v6 (Emerald Rapids, memory-optimized) ---
        // Not in CCF Azure CSV 2026-04-24. SPECpower direct compute
        // (0.55/3.20 W/vCPU) + DRAM premium. Final 0.71/3.60.
        ("Standard_E2s_v6", 1.4, 7.2),
        ("Standard_E4s_v6", 2.8, 14.4),
        ("Standard_E8s_v6", 5.7, 28.8),
        ("Standard_E16s_v6", 11.4, 57.6),
        ("Standard_E32s_v6", 22.7, 115.2),
        ("Standard_E64s_v6", 45.4, 230.4),
        ("Standard_E96s_v6", 68.2, 345.6),
        // --- xeon-6780e (Sierra Forest 144 E-core, 1-chip system level) ---
        // ASSUMES FULL CHIP OWNERSHIP. For partial-vCPU bare-metal,
        // override via [green.cloud.services.X] idle_watts/max_watts.
        ("xeon-6780e", 100.0, 420.0),
    ];
    let mut m = HashMap::with_capacity(entries.len());
    for &(name, idle, max) in entries {
        m.insert(name, (idle, max));
    }
    m
});

/// Generic default `(idle_watts, max_watts)` per cloud provider.
///
/// Used as a fallback when an instance type is not found in the
/// [`INSTANCE_POWER`] table. Values are approximate medians across
/// each provider's most common general-purpose 2-vCPU instances.
static PROVIDER_DEFAULTS: LazyLock<HashMap<&'static str, (f64, f64)>> = LazyLock::new(|| {
    let mut m = HashMap::with_capacity(4);
    // Defaults match the most common 2-vCPU general-purpose instance per
    // provider, on CCF 2026-04-24 per-vCPU coefficients where the CSV
    // publishes the architecture. Azure falls back to its v6 SPECpower
    // direct compute (CCF Azure CSV has no Emerald Rapids row). Operators
    // wanting a different default should set `default_instance_type`.
    m.insert("aws", (1.4, 8.1)); // m5.large (Cascade Lake, CCF)
    m.insert("gcp", (1.4, 7.5)); // n2-standard-2 (Cascade Lake, CCF)
    m.insert("azure", (1.1, 6.4)); // Standard_D2s_v6 (Emerald Rapids, SPECpower direct)
    m.insert("generic", (3.0, 20.0)); // Conservative on-prem server estimate
    m
});

/// Look up `(idle_watts, max_watts)` for an instance type.
///
/// Falls back to the provider default if the instance type is not in
/// the embedded table. Falls back to the `"generic"` default if the
/// provider is also unknown.
///
/// # Panics
///
/// Panics if the `"generic"` key is missing from `PROVIDER_DEFAULTS`
/// (compile-time invariant, cannot happen in practice).
#[must_use]
pub fn lookup_instance_power(instance_type: &str, provider: &str) -> (f64, f64) {
    if let Some(&power) = INSTANCE_POWER.get(instance_type) {
        return power;
    }
    if let Some(&power) = PROVIDER_DEFAULTS.get(provider) {
        return power;
    }
    // Ultimate fallback: generic on-prem estimate.
    *PROVIDER_DEFAULTS
        .get("generic")
        .expect("generic default must exist")
}

/// Returns `true` if the instance type is known in the embedded table.
#[must_use]
pub fn is_known_instance_type(instance_type: &str) -> bool {
    INSTANCE_POWER.contains_key(instance_type)
}

/// Linearly interpolate watts from CPU utilization percentage.
///
/// Formula: `idle_watts + (max_watts - idle_watts) * (cpu_percent / 100.0)`.
/// `cpu_percent` is clamped to `[0.0, 100.0]` to prevent extrapolation.
/// Non-finite inputs (NaN, infinity) return `idle_watts` as a safe default.
#[must_use]
pub fn interpolate_watts(idle_watts: f64, max_watts: f64, cpu_percent: f64) -> f64 {
    if !cpu_percent.is_finite() {
        return idle_watts;
    }
    let clamped = cpu_percent.clamp(0.0, 100.0);
    idle_watts + (max_watts - idle_watts) * (clamped / 100.0)
}

/// Compute energy per I/O op in kWh from interpolated watts, scrape
/// interval, and op count.
///
/// Returns `None` if `ops` is zero, watts is non-finite, or watts is
/// negative. The formula mirrors the Scaphandre integration:
/// `energy_kwh = (watts / 1000) * (interval_secs / 3600)`, then
/// divided by the number of ops in the window.
#[must_use]
pub fn compute_cloud_energy_per_op_kwh(
    watts: f64,
    scrape_interval_secs: f64,
    ops: u64,
) -> Option<f64> {
    if ops == 0 || !watts.is_finite() || watts < 0.0 {
        return None;
    }
    let kwh = (watts / 1000.0) * (scrape_interval_secs / 3600.0);
    let per_op = kwh / ops as f64;
    if per_op.is_finite() {
        Some(per_op)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // lookup_instance_power
    // ------------------------------------------------------------------

    #[test]
    fn known_aws_instance() {
        let (idle, max) = lookup_instance_power("m5.large", "aws");
        // m5.large (2 vCPU, Cascade Lake) at CCF 2026-04-24:
        // 2 * 0.690 = 1.4 idle, 2 * 4.063 = 8.1 max.
        assert!((idle - 1.4).abs() < 0.01);
        assert!((max - 8.1).abs() < 0.01);
    }

    #[test]
    fn known_gcp_instance() {
        let (idle, max) = lookup_instance_power("n2-standard-8", "gcp");
        // n2-standard-8 (8 vCPU, GCP Cascade Lake) at CCF 2026-04-24:
        // 8 * 0.690 = 5.5 idle, 8 * 3.755 = 30.0 max.
        assert!((idle - 5.5).abs() < 0.01);
        assert!((max - 30.0).abs() < 0.01);
    }

    #[test]
    fn known_azure_instance() {
        let (idle, max) = lookup_instance_power("Standard_D8s_v3", "azure");
        assert!((idle - 5.2).abs() < 0.01);
        assert!((max - 33.5).abs() < 0.1);
    }

    #[test]
    fn unknown_instance_falls_back_to_provider_default() {
        let (idle, max) = lookup_instance_power("m999.future", "aws");
        // AWS default matches m5.large under the homogeneous CCF 2026-04-24 methodology.
        assert!((idle - 1.4).abs() < 0.01);
        assert!((max - 8.1).abs() < 0.01);
    }

    #[test]
    fn modern_architecture_keys_present() {
        for key in [
            "m7i.large",
            "c7a.large",
            "r7a.large",
            "m6a.xlarge",
            "c7g.large",
            "m8g.large",
            "m8a.large",
            "c8a.large",
            "m8i.large",
            "c8i.large",
            "c4-standard-4",
            "c4d-standard-8",
            "c4a-standard-2",
            "t2a-standard-2",
            "Standard_D2s_v6",
            "Standard_D2ps_v6",
            "xeon-6780e",
        ] {
            assert!(is_known_instance_type(key), "missing modern entry: {key}");
        }
    }

    #[test]
    fn turin_overrides_to_genoa_proxy() {
        // EPYC 5th Gen Turin (m8a/c8a) is proxied to EPYC 4th Gen Genoa
        // (m7a/c7a) because the CCF 2026-04-24 Turin row is anomalously
        // high. If this test fails, re-evaluate the override against the
        // current CCF snapshot before silently aligning to a new value.
        // See docs/LIMITATIONS.md section "EPYC 5th Gen Turin" for the
        // rationale and the revalidation procedure.
        let sizes = [
            "large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "16xlarge",
        ];
        for size in sizes {
            for (turin, genoa) in [("m8a", "m7a"), ("c8a", "c7a")] {
                let turin_key = format!("{turin}.{size}");
                let genoa_key = format!("{genoa}.{size}");
                assert_eq!(
                    lookup_instance_power(&turin_key, "aws"),
                    lookup_instance_power(&genoa_key, "aws"),
                    "{turin_key} (Turin) must alias to {genoa_key} (Genoa) until CCF correction"
                );
            }
        }
    }

    #[test]
    fn m_series_does_not_carry_dram_premium() {
        // General-purpose m5.large (Cascade Lake) must remain on the bare
        // CCF coefficient (2 vCPU * 0.690 / 4.063), without any DRAM
        // premium uplift. If a future refactor accidentally applies the
        // premium to general-purpose families, the methodology departure
        // count documented in `table.rs` head doc-comment would silently
        // grow and the LIMITATIONS note would be wrong.
        let (idle, max) = lookup_instance_power("m5.large", "aws");
        assert!(
            (idle - 1.4).abs() < 0.05,
            "m5.large idle drifted: {idle} expected ~1.4"
        );
        assert!(
            (max - 8.1).abs() < 0.1,
            "m5.large max drifted: {max} expected ~8.1"
        );
    }

    #[test]
    fn r_series_includes_dram_premium_over_general_purpose() {
        // r5.large (2 vCPU memory-optimized, Cascade Lake) should carry
        // an additive DRAM premium over m5.large (2 vCPU general-purpose,
        // same Cascade Lake): 2 vCPU * 8 GB/vCPU * 0.02 W/GB = 0.32 idle,
        // 2 vCPU * 8 GB/vCPU * 0.05 W/GB = 0.80 max. If this test fails,
        // re-check that DRAM_PREMIUM_W_PER_GB_{IDLE,MAX} and the inline
        // r-series values stay in sync.
        let (m5_idle, m5_max) = lookup_instance_power("m5.large", "aws");
        let (r5_idle, r5_max) = lookup_instance_power("r5.large", "aws");
        assert!(
            (r5_idle - m5_idle - 0.32).abs() < 0.05,
            "DRAM idle uplift drift: r5 {r5_idle} - m5 {m5_idle} expected ~0.32"
        );
        assert!(
            (r5_max - m5_max - 0.80).abs() < 0.05,
            "DRAM max uplift drift: r5 {r5_max} - m5 {m5_max} expected ~0.80"
        );
    }

    #[test]
    fn sierra_forest_entries_are_chip_level_not_vcpu_level() {
        // Sierra Forest entries are 1-chip system-level watts, not vCPU-scaled.
        // A vCPU-scaled value would never exceed ~6 W idle for a 144-thread
        // entry, so floor at 50 W idle catches accidental rescaling errors.
        let (idle, _) = lookup_instance_power("xeon-6780e", "generic");
        assert!(
            idle >= 50.0,
            "xeon-6780e must be system-level (>=50W idle), got {idle}"
        );
    }

    #[test]
    fn unknown_provider_falls_back_to_generic() {
        let (idle, max) = lookup_instance_power("custom.instance", "onprem");
        assert!((idle - 3.0).abs() < 0.01);
        assert!((max - 20.0).abs() < 0.01);
    }

    #[test]
    fn is_known_true_for_table_entry() {
        assert!(is_known_instance_type("c5.4xlarge"));
    }

    #[test]
    fn is_known_false_for_missing_entry() {
        assert!(!is_known_instance_type("m99.jumbo"));
    }

    // ------------------------------------------------------------------
    // interpolate_watts
    // ------------------------------------------------------------------

    #[test]
    fn interpolate_at_zero_percent() {
        let w = interpolate_watts(2.0, 20.0, 0.0);
        assert!((w - 2.0).abs() < 1e-10);
    }

    #[test]
    fn interpolate_at_fifty_percent() {
        let w = interpolate_watts(2.0, 20.0, 50.0);
        assert!((w - 11.0).abs() < 1e-10);
    }

    #[test]
    fn interpolate_at_hundred_percent() {
        let w = interpolate_watts(2.0, 20.0, 100.0);
        assert!((w - 20.0).abs() < 1e-10);
    }

    #[test]
    fn interpolate_clamps_below_zero() {
        let w = interpolate_watts(2.0, 20.0, -10.0);
        assert!((w - 2.0).abs() < 1e-10, "should clamp to idle");
    }

    #[test]
    fn interpolate_clamps_above_hundred() {
        let w = interpolate_watts(2.0, 20.0, 150.0);
        assert!((w - 20.0).abs() < 1e-10, "should clamp to max");
    }

    #[test]
    fn interpolate_nan_returns_idle() {
        let w = interpolate_watts(2.0, 20.0, f64::NAN);
        assert!((w - 2.0).abs() < 1e-10, "NaN input should return idle");
    }

    #[test]
    fn interpolate_infinity_returns_idle() {
        let w = interpolate_watts(2.0, 20.0, f64::INFINITY);
        assert!((w - 2.0).abs() < 1e-10, "Inf input should return idle");
    }

    // ------------------------------------------------------------------
    // compute_cloud_energy_per_op_kwh
    // ------------------------------------------------------------------

    #[test]
    fn basic_energy_computation() {
        // 10 W for 15 seconds, 100 ops.
        // kWh = 10/1000 * 15/3600 = 0.0000416667
        // per_op = 0.0000416667 / 100 = 4.16667e-7
        let result = compute_cloud_energy_per_op_kwh(10.0, 15.0, 100);
        assert!(result.is_some());
        let per_op = result.unwrap();
        let expected = (10.0 / 1000.0) * (15.0 / 3600.0) / 100.0;
        assert!((per_op - expected).abs() < 1e-15);
    }

    #[test]
    fn zero_ops_returns_none() {
        assert!(compute_cloud_energy_per_op_kwh(10.0, 15.0, 0).is_none());
    }

    #[test]
    fn negative_watts_returns_none() {
        assert!(compute_cloud_energy_per_op_kwh(-1.0, 15.0, 100).is_none());
    }

    #[test]
    fn nan_watts_returns_none() {
        assert!(compute_cloud_energy_per_op_kwh(f64::NAN, 15.0, 100).is_none());
    }

    #[test]
    fn infinite_watts_returns_none() {
        assert!(compute_cloud_energy_per_op_kwh(f64::INFINITY, 15.0, 100).is_none());
    }

    // ------------------------------------------------------------------
    // Table integrity
    // ------------------------------------------------------------------

    #[test]
    fn all_entries_have_positive_values() {
        for (name, &(idle, max)) in INSTANCE_POWER.iter() {
            assert!(idle > 0.0, "{name}: idle must be positive, got {idle}");
            assert!(max > 0.0, "{name}: max must be positive, got {max}");
            assert!(max >= idle, "{name}: max ({max}) must be >= idle ({idle})");
        }
    }

    #[test]
    fn table_has_expected_entry_count() {
        // CCF 2026-04-24 vCPU * coefficient across AWS/GCP/Azure, plus a
        // handful of direct-SPECpower entries where the provider CSV has
        // no row (Azure v6, GCP t2a/c4d, Cobalt 100), plus 1 Sierra Forest
        // CPU-named system-level row. Conservative floor so the count
        // survives minor entry pruning during review.
        assert!(
            INSTANCE_POWER.len() >= 300,
            "expected >= 300 entries, got {}",
            INSTANCE_POWER.len()
        );
    }
}
