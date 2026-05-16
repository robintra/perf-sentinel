//! Embedded `SPECpower` instance type lookup table.
//!
//! Maps cloud instance types to their `(idle_watts, max_watts)` envelope.
//! Two data vintages coexist with different methodologies:
//!
//! - **Legacy entries** (Cascade Lake, Skylake, Ice Lake, EPYC 1st-3rd
//!   Gen, ~190 instances): from [Cloud Carbon Footprint coefficients]
//!   2023-05-01, AWS direct from `aws-instances.csv` (baseboard inclus),
//!   GCP and Azure as `vCPU * coefficient`.
//! - **Modern entries** (Sapphire Rapids and beyond, ~130 instances):
//!   `SPECpower_ssj 2008` quarterly results 2024 Q1 - 2026 Q2, computed
//!   as `vCPU * (avg_watts_at_load / total_threads)` averaged per
//!   architecture. No AWS-specific baseboard overhead is layered on top.
//!
//! Modern AWS entries therefore read lower than legacy AWS entries of
//! the same nominal size. The Graviton 3/4 and Cobalt 100 ARM entries
//! are estimates (no `SPECpower` submissions) bounded by Ampere Altra
//! (floor) and Sapphire Rapids minus 25% (AWS public claim, upper).
//! Sierra Forest entries (`xeon-6780e`) are 1-chip system level for
//! bare-metal users owning the full chip -- not vCPU-scaled.
//!
//! Full methodology, uncertainty bounds, and per-architecture caveats
//! are in `docs/LIMITATIONS.md` "Cloud `SPECpower` precision bounds".
//!
//! [Cloud Carbon Footprint coefficients]: https://github.com/cloud-carbon-footprint/cloud-carbon-coefficients

use std::collections::HashMap;
use std::sync::LazyLock;

/// Vintage of the modern `SPECpower` entries (Sapphire Rapids and beyond)
/// in this table. Release procedure step 2.5 surfaces this string via
/// `grep`. Bump when the `SPECpower` quarterly results are refreshed.
#[allow(dead_code)]
pub(crate) const SPECPOWER_VINTAGE: &str = "2024 Q1 - 2026 Q2";

/// `(idle_watts, max_watts)` per instance type.
///
/// Idle watts represent power at near-zero CPU load. Max watts represent
/// power at 100% CPU utilization. Both include CPU, memory, and
/// baseboard overhead proportional to the instance's share of the host.
static INSTANCE_POWER: LazyLock<HashMap<&'static str, (f64, f64)>> = LazyLock::new(|| {
    let entries: &[(&str, f64, f64)] = &[
        // ================================================================
        // AWS instances (direct from CCF aws-instances.csv)
        // ================================================================

        // --- t3 (Nitro, Cascade Lake / Skylake, burstable) ---
        ("t3.nano", 2.0, 12.5),
        ("t3.micro", 2.0, 13.0),
        ("t3.small", 2.0, 14.0),
        ("t3.medium", 2.0, 16.0),
        ("t3.large", 2.0, 20.0),
        ("t3.xlarge", 4.0, 39.9),
        ("t3.2xlarge", 8.0, 79.8),
        // --- t3a (Nitro, EPYC 1st Gen, burstable) ---
        ("t3a.nano", 1.7, 10.5),
        ("t3a.micro", 1.7, 10.8),
        ("t3a.small", 1.7, 11.4),
        ("t3a.medium", 1.7, 12.6),
        ("t3a.large", 1.7, 15.0),
        ("t3a.xlarge", 3.3, 29.9),
        ("t3a.2xlarge", 6.7, 59.8),
        // --- m5 (Cascade Lake / Skylake, general purpose) ---
        ("m5.large", 2.0, 20.0),
        ("m5.xlarge", 4.0, 39.9),
        ("m5.2xlarge", 8.0, 79.8),
        ("m5.4xlarge", 16.0, 159.6),
        ("m5.8xlarge", 32.0, 319.3),
        ("m5.12xlarge", 48.0, 478.9),
        ("m5.16xlarge", 64.0, 638.5),
        ("m5.24xlarge", 96.0, 957.8),
        // --- m5a (EPYC 1st Gen, general purpose) ---
        ("m5a.large", 1.7, 15.0),
        ("m5a.xlarge", 3.3, 29.9),
        ("m5a.2xlarge", 6.7, 59.8),
        ("m5a.4xlarge", 13.3, 119.6),
        ("m5a.8xlarge", 26.7, 239.3),
        ("m5a.12xlarge", 40.0, 358.9),
        ("m5a.16xlarge", 53.3, 478.5),
        ("m5a.24xlarge", 80.0, 717.8),
        // --- c5 (Cascade Lake / Skylake, compute-optimized) ---
        ("c5.large", 2.7, 18.0),
        ("c5.xlarge", 5.3, 35.9),
        ("c5.2xlarge", 10.7, 71.9),
        ("c5.4xlarge", 21.3, 143.7),
        ("c5.9xlarge", 48.0, 323.4),
        ("c5.12xlarge", 48.0, 466.6),
        ("c5.18xlarge", 96.0, 646.8),
        ("c5.24xlarge", 96.0, 933.2),
        // --- c5a (EPYC 2nd Gen, compute-optimized) ---
        ("c5a.large", 1.2, 9.5),
        ("c5a.xlarge", 2.3, 19.0),
        ("c5a.2xlarge", 4.7, 38.0),
        ("c5a.4xlarge", 9.3, 76.1),
        ("c5a.8xlarge", 18.7, 152.1),
        ("c5a.12xlarge", 28.0, 228.2),
        ("c5a.16xlarge", 37.3, 304.3),
        ("c5a.24xlarge", 56.0, 456.4),
        // --- r5 (Cascade Lake / Skylake, memory-optimized) ---
        ("r5.large", 2.0, 27.9),
        ("r5.xlarge", 4.0, 55.9),
        ("r5.2xlarge", 8.0, 111.8),
        ("r5.4xlarge", 16.0, 223.6),
        ("r5.8xlarge", 32.0, 447.2),
        ("r5.12xlarge", 48.0, 670.8),
        ("r5.16xlarge", 64.0, 894.4),
        ("r5.24xlarge", 96.0, 1341.6),
        // --- r5a (EPYC 1st Gen, memory-optimized) ---
        ("r5a.large", 1.7, 19.8),
        ("r5a.xlarge", 3.3, 39.5),
        ("r5a.2xlarge", 6.7, 79.0),
        ("r5a.4xlarge", 13.3, 158.0),
        ("r5a.8xlarge", 26.7, 316.1),
        ("r5a.12xlarge", 40.0, 474.1),
        ("r5a.16xlarge", 53.3, 632.1),
        ("r5a.24xlarge", 80.0, 948.2),
        // --- m6i (Ice Lake, general purpose) ---
        ("m6i.large", 1.9, 16.2),
        ("m6i.xlarge", 3.8, 32.4),
        ("m6i.2xlarge", 7.5, 64.9),
        ("m6i.4xlarge", 15.0, 129.8),
        ("m6i.8xlarge", 30.0, 259.6),
        ("m6i.12xlarge", 45.0, 389.4),
        ("m6i.16xlarge", 60.0, 519.2),
        ("m6i.24xlarge", 90.0, 778.7),
        ("m6i.32xlarge", 120.0, 1038.3),
        // --- c6i (Ice Lake, compute-optimized) ---
        // Derived from m6i per-vCPU ratio (0.95 / 8.1 W/vCPU).
        ("c6i.large", 1.9, 16.2),
        ("c6i.xlarge", 3.8, 32.4),
        ("c6i.2xlarge", 7.5, 64.9),
        ("c6i.4xlarge", 15.0, 129.8),
        ("c6i.8xlarge", 30.0, 259.6),
        ("c6i.12xlarge", 45.0, 389.4),
        ("c6i.16xlarge", 60.0, 519.2),
        ("c6i.24xlarge", 90.0, 778.7),
        ("c6i.32xlarge", 120.0, 1038.3),
        // --- r6i (Ice Lake, memory-optimized) ---
        // Derived from m6i per-vCPU ratio with memory overhead factor
        // (~1.15x max from r5/m5 ratio).
        ("r6i.large", 1.9, 18.6),
        ("r6i.xlarge", 3.8, 37.3),
        ("r6i.2xlarge", 7.5, 74.6),
        ("r6i.4xlarge", 15.0, 149.3),
        ("r6i.8xlarge", 30.0, 298.5),
        ("r6i.12xlarge", 45.0, 447.8),
        ("r6i.16xlarge", 60.0, 597.1),
        ("r6i.24xlarge", 90.0, 895.6),
        // --- m7i (Sapphire Rapids, Xeon Platinum 8488C) ---
        // SPECpower 2024 Q1-Q2, n=6 Platinum 8480+/8490H, 0.71/3.50 W/vCPU
        ("m7i.large", 1.4, 7.0),
        ("m7i.xlarge", 2.8, 14.0),
        ("m7i.2xlarge", 5.7, 28.0),
        ("m7i.4xlarge", 11.4, 56.0),
        ("m7i.8xlarge", 22.7, 112.0),
        ("m7i.16xlarge", 45.4, 224.0),
        // --- c7i (Sapphire Rapids, compute-optimized) ---
        ("c7i.large", 1.4, 7.0),
        ("c7i.xlarge", 2.8, 14.0),
        ("c7i.2xlarge", 5.7, 28.0),
        ("c7i.4xlarge", 11.4, 56.0),
        ("c7i.8xlarge", 22.7, 112.0),
        ("c7i.16xlarge", 45.4, 224.0),
        // --- r7i (Sapphire Rapids, memory-optimized, 1.15x max factor) ---
        ("r7i.large", 1.4, 8.0),
        ("r7i.xlarge", 2.8, 16.1),
        ("r7i.2xlarge", 5.7, 32.2),
        ("r7i.4xlarge", 11.4, 64.4),
        ("r7i.8xlarge", 22.7, 128.8),
        ("r7i.16xlarge", 45.4, 257.6),
        // --- m7a (AMD Genoa, EPYC 9R14 custom) ---
        // SPECpower 2024 Q1, EPYC 9654 + extrapolation, 0.40/2.05 W/vCPU
        ("m7a.large", 0.8, 4.1),
        ("m7a.xlarge", 1.6, 8.2),
        ("m7a.2xlarge", 3.2, 16.4),
        ("m7a.4xlarge", 6.4, 32.8),
        ("m7a.8xlarge", 12.8, 65.6),
        ("m7a.16xlarge", 25.6, 131.2),
        // --- c7a (Genoa, compute-optimized) ---
        ("c7a.large", 0.8, 4.1),
        ("c7a.xlarge", 1.6, 8.2),
        ("c7a.2xlarge", 3.2, 16.4),
        ("c7a.4xlarge", 6.4, 32.8),
        ("c7a.8xlarge", 12.8, 65.6),
        ("c7a.16xlarge", 25.6, 131.2),
        // --- m6a (AMD Milan, EPYC 7R13) ---
        // CCF EPYC 3rd Gen coefficient, 0.445/2.019 W/vCPU
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
        // Mix B+D: floor Ampere Altra (0.67/1.75), upper SPR x 0.75 = 0.53/2.63 W/vCPU
        ("m7g.large", 1.1, 5.3),
        ("m7g.xlarge", 2.1, 10.5),
        ("m7g.2xlarge", 4.2, 21.0),
        ("m7g.4xlarge", 8.5, 42.1),
        ("m7g.8xlarge", 17.0, 84.2),
        ("m7g.16xlarge", 33.9, 168.3),
        // --- c7g (Graviton 3, compute-optimized) ---
        ("c7g.large", 1.1, 5.3),
        ("c7g.xlarge", 2.1, 10.5),
        ("c7g.2xlarge", 4.2, 21.0),
        ("c7g.4xlarge", 8.5, 42.1),
        ("c7g.8xlarge", 17.0, 84.2),
        ("c7g.16xlarge", 33.9, 168.3),
        // --- m8g (Graviton 4, Neoverse V2) ---
        // Graviton 3 x 0.70 per AWS re:Invent 2023 claim, 0.37/1.84 W/vCPU
        ("m8g.large", 0.7, 3.7),
        ("m8g.xlarge", 1.5, 7.4),
        ("m8g.2xlarge", 3.0, 14.7),
        ("m8g.4xlarge", 5.9, 29.4),
        ("m8g.8xlarge", 11.8, 58.9),
        ("m8g.16xlarge", 23.7, 117.8),
        // --- c8g (Graviton 4, compute-optimized) ---
        ("c8g.large", 0.7, 3.7),
        ("c8g.xlarge", 1.5, 7.4),
        ("c8g.2xlarge", 3.0, 14.7),
        ("c8g.4xlarge", 5.9, 29.4),
        ("c8g.8xlarge", 11.8, 58.9),
        ("c8g.16xlarge", 23.7, 117.8),
        // ================================================================
        // GCP instances (vCPU * per_vCPU_coefficient from CCF)
        // ================================================================

        // --- n2-standard (Cascade Lake, general purpose) ---
        // CCF Cascade Lake: 0.638 min / 3.642 max per vCPU
        ("n2-standard-2", 1.3, 7.3),
        ("n2-standard-4", 2.6, 14.6),
        ("n2-standard-8", 5.1, 29.1),
        ("n2-standard-16", 10.2, 58.3),
        ("n2-standard-32", 20.4, 116.5),
        ("n2-standard-48", 30.6, 174.8),
        ("n2-standard-64", 40.8, 233.1),
        ("n2-standard-80", 51.0, 291.4),
        ("n2-standard-96", 61.2, 349.6),
        ("n2-standard-128", 81.7, 466.2),
        // --- n2-highcpu (Cascade Lake, compute-optimized) ---
        ("n2-highcpu-2", 1.3, 7.3),
        ("n2-highcpu-4", 2.6, 14.6),
        ("n2-highcpu-8", 5.1, 29.1),
        ("n2-highcpu-16", 10.2, 58.3),
        ("n2-highcpu-32", 20.4, 116.5),
        ("n2-highcpu-48", 30.6, 174.8),
        ("n2-highcpu-64", 40.8, 233.1),
        ("n2-highcpu-80", 51.0, 291.4),
        ("n2-highcpu-96", 61.2, 349.6),
        // --- n2-highmem (Cascade Lake, memory-optimized) ---
        ("n2-highmem-2", 1.3, 7.3),
        ("n2-highmem-4", 2.6, 14.6),
        ("n2-highmem-8", 5.1, 29.1),
        ("n2-highmem-16", 10.2, 58.3),
        ("n2-highmem-32", 20.4, 116.5),
        ("n2-highmem-48", 30.6, 174.8),
        ("n2-highmem-64", 40.8, 233.1),
        ("n2-highmem-80", 51.0, 291.4),
        ("n2-highmem-96", 61.2, 349.6),
        ("n2-highmem-128", 81.7, 466.2),
        // --- e2-standard (EPYC 2nd Gen / Skylake mix, general purpose) ---
        // CCF EPYC 2nd Gen: 0.474 min / 1.575 max per vCPU
        ("e2-standard-2", 0.9, 3.2),
        ("e2-standard-4", 1.9, 6.3),
        ("e2-standard-8", 3.8, 12.6),
        ("e2-standard-16", 7.6, 25.2),
        ("e2-standard-32", 15.2, 50.4),
        // --- c2-standard (Cascade Lake, compute-optimized) ---
        ("c2-standard-4", 2.6, 14.6),
        ("c2-standard-8", 5.1, 29.1),
        ("c2-standard-16", 10.2, 58.3),
        ("c2-standard-30", 19.1, 109.3),
        ("c2-standard-60", 38.3, 218.5),
        // --- c3 (Sapphire Rapids, general purpose) ---
        // SPECpower 2024 Q1-Q2, Xeon Platinum 8480+/8490H, 0.71/3.50 W/vCPU
        ("c3-standard-4", 2.8, 14.0),
        ("c3-standard-8", 5.7, 28.0),
        ("c3-standard-22", 15.6, 77.0),
        ("c3-standard-44", 31.2, 154.0),
        ("c3-standard-88", 62.5, 308.0),
        ("c3-standard-176", 125.0, 616.0),
        // --- c3d (AMD Genoa, EPYC 9004) ---
        // SPECpower 2024 Q1 EPYC 9654 + extrapolation, 0.40/2.05 W/vCPU
        ("c3d-standard-4", 1.6, 8.2),
        ("c3d-standard-8", 3.2, 16.4),
        ("c3d-standard-16", 6.4, 32.8),
        ("c3d-standard-30", 12.0, 61.5),
        ("c3d-standard-60", 24.0, 123.0),
        ("c3d-standard-180", 72.0, 369.0),
        // --- c4 (Emerald Rapids, Xeon Platinum 8592+) ---
        // SPECpower 2024 Q1-Q2, n=18 Platinum 8592+/8581V, 0.55/3.20 W/vCPU
        ("c4-standard-2", 1.1, 6.4),
        ("c4-standard-4", 2.2, 12.8),
        ("c4-standard-8", 4.4, 25.6),
        ("c4-standard-16", 8.8, 51.2),
        ("c4-standard-32", 17.6, 102.4),
        ("c4-standard-96", 52.8, 307.2),
        // --- c4d (AMD Turin, EPYC 9005 Zen 5) ---
        // SPECpower 2024 Q4-2026 Q2, n=9 EPYC 9655/9755, 0.32/1.91 W/vCPU
        ("c4d-standard-2", 0.6, 3.8),
        ("c4d-standard-4", 1.3, 7.6),
        ("c4d-standard-8", 2.6, 15.3),
        ("c4d-standard-16", 5.1, 30.6),
        ("c4d-standard-32", 10.2, 61.1),
        ("c4d-standard-96", 30.7, 183.4),
        // --- n2d (Genoa-era newer, EPYC 9004) ---
        ("n2d-standard-2", 0.8, 4.1),
        ("n2d-standard-4", 1.6, 8.2),
        ("n2d-standard-8", 3.2, 16.4),
        ("n2d-standard-16", 6.4, 32.8),
        ("n2d-standard-32", 12.8, 65.6),
        ("n2d-standard-64", 25.6, 131.2),
        // --- t2a (Ampere Altra, Neoverse N1) ---
        // SPECpower 2024 Q1, n=1 Altra Q80-30, 0.67/1.75 W/vCPU
        ("t2a-standard-1", 0.7, 1.8),
        ("t2a-standard-2", 1.3, 3.5),
        ("t2a-standard-4", 2.7, 7.0),
        ("t2a-standard-8", 5.4, 14.0),
        ("t2a-standard-16", 10.7, 28.0),
        ("t2a-standard-32", 21.4, 56.0),
        // ================================================================
        // Azure instances (vCPU * per_vCPU_coefficient from CCF)
        // ================================================================

        // --- Standard_D v3 (Broadwell / Skylake) ---
        // CCF Skylake: 0.645 min / 4.193 max per vCPU
        ("Standard_D2s_v3", 1.3, 8.4),
        ("Standard_D4s_v3", 2.6, 16.8),
        ("Standard_D8s_v3", 5.2, 33.5),
        ("Standard_D16s_v3", 10.3, 67.1),
        ("Standard_D32s_v3", 20.6, 134.2),
        ("Standard_D48s_v3", 31.0, 201.3),
        ("Standard_D64s_v3", 41.3, 268.4),
        // --- Standard_D v4 (Cascade Lake) ---
        // CCF Cascade Lake: 0.639 min / 3.967 max per vCPU
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
        // CCF EPYC 3rd Gen: 0.445 min / 2.019 max per vCPU
        ("Standard_D2as_v5", 0.9, 4.0),
        ("Standard_D4as_v5", 1.8, 8.1),
        ("Standard_D8as_v5", 3.6, 16.2),
        ("Standard_D16as_v5", 7.1, 32.3),
        ("Standard_D32as_v5", 14.2, 64.6),
        ("Standard_D48as_v5", 21.4, 96.9),
        ("Standard_D64as_v5", 28.5, 129.2),
        ("Standard_D96as_v5", 42.7, 193.8),
        // --- Standard_E v3 (Broadwell / Skylake, memory-optimized) ---
        ("Standard_E2s_v3", 1.3, 8.4),
        ("Standard_E4s_v3", 2.6, 16.8),
        ("Standard_E8s_v3", 5.2, 33.5),
        ("Standard_E16s_v3", 10.3, 67.1),
        ("Standard_E32s_v3", 20.6, 134.2),
        ("Standard_E48s_v3", 31.0, 201.3),
        ("Standard_E64s_v3", 41.3, 268.4),
        // --- Standard_E v4 (Cascade Lake, memory-optimized) ---
        ("Standard_E2s_v4", 1.3, 7.9),
        ("Standard_E4s_v4", 2.6, 15.9),
        ("Standard_E8s_v4", 5.1, 31.7),
        ("Standard_E16s_v4", 10.2, 63.5),
        ("Standard_E32s_v4", 20.4, 126.9),
        ("Standard_E48s_v4", 30.7, 190.4),
        ("Standard_E64s_v4", 40.9, 253.9),
        // --- Standard_E v5 (Cascade Lake / Ice Lake, memory-optimized) ---
        ("Standard_E2s_v5", 1.3, 7.9),
        ("Standard_E4s_v5", 2.6, 15.9),
        ("Standard_E8s_v5", 5.1, 31.7),
        ("Standard_E16s_v5", 10.2, 63.5),
        ("Standard_E32s_v5", 20.4, 126.9),
        ("Standard_E48s_v5", 30.7, 190.4),
        ("Standard_E64s_v5", 40.9, 253.9),
        ("Standard_E96s_v5", 61.3, 380.8),
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
        // SPECpower 2024 Q1-Q2, n=18 Platinum 8592+/8581V, 0.55/3.20 W/vCPU
        ("Standard_D2s_v6", 1.1, 6.4),
        ("Standard_D4s_v6", 2.2, 12.8),
        ("Standard_D8s_v6", 4.4, 25.6),
        ("Standard_D16s_v6", 8.8, 51.2),
        ("Standard_D32s_v6", 17.6, 102.4),
        ("Standard_D64s_v6", 35.2, 204.8),
        ("Standard_D96s_v6", 52.8, 307.2),
        // --- Standard_Dads v6 (AMD Genoa, EPYC 9004) ---
        // 0.40/2.05 W/vCPU per Genoa coefficient
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
        // Same coefficient as Dv6 by Azure convention (no memory premium applied)
        ("Standard_E2s_v6", 1.1, 6.4),
        ("Standard_E4s_v6", 2.2, 12.8),
        ("Standard_E8s_v6", 4.4, 25.6),
        ("Standard_E16s_v6", 8.8, 51.2),
        ("Standard_E32s_v6", 17.6, 102.4),
        ("Standard_E64s_v6", 35.2, 204.8),
        ("Standard_E96s_v6", 52.8, 307.2),
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
    // AWS default kept on legacy m5.large to preserve waste-signal continuity:
    // bumping to m7i would silently drop reported energy ~3x for unconfigured
    // services due to the methodology shift (legacy CCF baseboard vs modern
    // per-vCPU). Users wanting a modern default should set default_instance_type
    // explicitly. Azure bump is methodology-homogeneous (v4/v6 both per-vCPU).
    m.insert("aws", (2.0, 20.0)); // m5.large (Cascade Lake, CCF baseboard)
    m.insert("gcp", (1.3, 7.3)); // n2-standard-2 (Cascade Lake)
    m.insert("azure", (1.1, 6.4)); // Standard_D2s_v6 (Emerald Rapids)
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
        assert!((idle - 2.0).abs() < 0.01);
        assert!((max - 20.0).abs() < 0.01);
    }

    #[test]
    fn known_gcp_instance() {
        let (idle, max) = lookup_instance_power("n2-standard-8", "gcp");
        assert!((idle - 5.1).abs() < 0.01);
        assert!((max - 29.1).abs() < 0.01);
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
        // AWS default kept on m5.large to preserve waste-signal continuity.
        assert!((idle - 2.0).abs() < 0.01);
        assert!((max - 20.0).abs() < 0.01);
    }

    #[test]
    fn modern_architecture_keys_present() {
        for key in [
            "m7i.large",
            "c7a.large",
            "m6a.xlarge",
            "c7g.large",
            "m8g.large",
            "c4-standard-4",
            "c4d-standard-8",
            "t2a-standard-2",
            "Standard_D2s_v6",
            "Standard_D2ps_v6",
            "xeon-6780e",
        ] {
            assert!(is_known_instance_type(key), "missing modern entry: {key}");
        }
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
        // 187 legacy + ~130 modern (2024-2026) + 1 Sierra Forest CPU-named.
        // Conservative floor: trims survive minor entry pruning during review.
        assert!(
            INSTANCE_POWER.len() >= 300,
            "expected >= 300 entries, got {}",
            INSTANCE_POWER.len()
        );
    }
}
