// GENERATED FILE - DO NOT EDIT BY HAND.
// Regenerate with: python3 scripts/refresh-instance-power.py
//
// `(instance_type, idle_watts, max_watts)` rows derived from the CCF
// per-architecture coefficients (`coefficients-{aws,gcp,azure}-use.csv`),
// snapshot 2026-04-24:
// https://github.com/cloud-carbon-footprint/ccf-coefficients/tree/b0032d928c78
//
// idle_watts = vCPU * idle_per_vcpu_coefficient (same for max).
// Memory-optimized families add the DRAM premium (+0.16/+0.40 per
// vCPU). Entries absent from the CCF CSVs live in
// `MANUAL_INSTANCE_ROWS` (table.rs). Full methodology in `table.rs`.

/// Surfaced in disclosure reports via `embedded_specpower_vintage` and
/// grep-audited by release procedure step 2.5. Stamped with the
/// ccf-coefficients HEAD commit date by the refresh script.
pub(crate) const SPECPOWER_VINTAGE: &str = "2026-04-24 (CCF aligned)";

pub(super) static GENERATED_INSTANCE_ROWS: &[(&str, f64, f64)] = &[
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
    // Turin is proxied to the CCF EPYC 4th Gen Genoa coefficient
    // (0.739/2.282); rationale in the `table.rs` module docs and
    // docs/LIMITATIONS.md.
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
    // --- n2d (Genoa-era newer, EPYC 9004) ---
    // CCF GCP EPYC 4th Gen: 0.739 idle / 2.196 max W/vCPU
    // (REFRESH: SPECpower direct compute diverged ~85% idle / ~7% max).
    ("n2d-standard-2", 1.5, 4.4),
    ("n2d-standard-4", 3.0, 8.8),
    ("n2d-standard-8", 5.9, 17.6),
    ("n2d-standard-16", 11.8, 35.1),
    ("n2d-standard-32", 23.7, 70.3),
    ("n2d-standard-64", 47.3, 140.5),
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
    // coefficients-azure-use.csv). The v6 families and Cobalt/Altra ARM
    // entries absent from this CSV live in `MANUAL_INSTANCE_ROWS`
    // (table.rs).
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
];
