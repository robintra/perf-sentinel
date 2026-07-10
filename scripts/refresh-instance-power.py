#!/usr/bin/env python3
"""Regenerate cloud_energy/table_data.rs from CCF coefficients.

Downloads coefficients-{aws,gcp,azure}-use.csv from the
cloud-carbon-footprint/ccf-coefficients repo and rewrites the generated
instance power rows (idle/max watts = vCPU * per-vCPU coefficient,
DRAM premium for memory-optimized families). Entries absent from the
CSVs stay in MANUAL_INSTANCE_ROWS (table.rs) and are not touched.

Usage:
    python3 scripts/refresh-instance-power.py [git-ref]

git-ref defaults to main. Pass a commit SHA for a reproducible run.
Prints changed rows on stdout and warnings on stderr. Exits 0 even when
values changed (the CI workflow decides via git diff), 1 on download or
parse failure. Run `cargo fmt -p perf-sentinel-core` afterwards (the CI
workflow does it). Run twice: the second run plus fmt must be a no-op.
"""

import csv
import io
import json
import os
import re
import sys
import urllib.request
from pathlib import Path

CCF_REPO = "cloud-carbon-footprint/ccf-coefficients"
REPO_ROOT = Path(__file__).resolve().parent.parent
TARGET = (
    REPO_ROOT / "crates" / "sentinel-core" / "src" / "score"
    / "cloud_energy" / "table_data.rs"
)

DRAM_IDLE, DRAM_MAX = 0.16, 0.40

AWS_VCPU = {
    "nano": 2, "micro": 2, "small": 2, "medium": 2, "large": 2,
    "xlarge": 4, "2xlarge": 8, "4xlarge": 16, "8xlarge": 32,
    "9xlarge": 36, "12xlarge": 48, "16xlarge": 64, "18xlarge": 72,
    "24xlarge": 96, "32xlarge": 128,
}

T_SIZES = ["nano", "micro", "small", "medium", "large", "xlarge", "2xlarge"]
M5_SIZES = ["large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "12xlarge", "16xlarge", "24xlarge"]
M6I_SIZES = M5_SIZES + ["32xlarge"]
M7_SIZES = ["large", "xlarge", "2xlarge", "4xlarge", "8xlarge", "16xlarge"]


def aws(prefix: str, sizes: "list[str]") -> "list[tuple[str, int]]":
    return [(f"{prefix}.{s}", AWS_VCPU[s]) for s in sizes]


def gcp(prefix: str, sizes: "list[int]") -> "list[tuple[str, int]]":
    return [(f"{prefix}-{n}", n) for n in sizes]


def azure(pattern: str, sizes: "list[int]") -> "list[tuple[str, int]]":
    return [(pattern.format(n), n) for n in sizes]


# One entry per family: (label, csv provider, CSV architecture, dram
# premium, instances, extra note lines). Adding a family means adding a
# line here, by hand. The Turin families deliberately map to the Genoa
# architecture, see docs/LIMITATIONS.md.
FAMILIES = [
    ("t3 (Nitro, Cascade Lake, burstable)", "aws", "Cascade Lake", False, aws("t3", T_SIZES),
     ["Burst credit is not modeled by CCF, sizes below xlarge count as 2 vCPU."]),
    ("t3a (Nitro, EPYC 1st Gen Naples, burstable)", "aws", "EPYC 1st Gen", False, aws("t3a", T_SIZES), []),
    ("m5 (Cascade Lake, general purpose)", "aws", "Cascade Lake", False, aws("m5", M5_SIZES), []),
    ("m5a (EPYC 1st Gen Naples, general purpose)", "aws", "EPYC 1st Gen", False, aws("m5a", M5_SIZES), []),
    ("c5 (Cascade Lake, compute-optimized)", "aws", "Cascade Lake", False,
     aws("c5", ["large", "xlarge", "2xlarge", "4xlarge", "9xlarge", "12xlarge", "18xlarge", "24xlarge"]),
     ["CCF does not differentiate compute vs general purpose."]),
    ("c5a (EPYC 2nd Gen Rome, compute-optimized)", "aws", "EPYC 2nd Gen", False, aws("c5a", M5_SIZES), []),
    ("r5 (Cascade Lake, memory-optimized)", "aws", "Cascade Lake", True, aws("r5", M5_SIZES), []),
    ("r5a (EPYC 1st Gen Naples, memory-optimized)", "aws", "EPYC 1st Gen", True, aws("r5a", M5_SIZES), []),
    ("m6i (Ice Lake, general purpose)", "aws", "Ice Lake", False, aws("m6i", M6I_SIZES), []),
    ("c6i (Ice Lake, compute-optimized)", "aws", "Ice Lake", False, aws("c6i", M6I_SIZES), []),
    ("r6i (Ice Lake, memory-optimized)", "aws", "Ice Lake", True, aws("r6i", M5_SIZES), []),
    ("m7i (Sapphire Rapids, Xeon Platinum 8488C)", "aws", "Sapphire Rapids", False, aws("m7i", M7_SIZES), []),
    ("c7i (Sapphire Rapids, compute-optimized)", "aws", "Sapphire Rapids", False, aws("c7i", M7_SIZES), []),
    ("r7i (Sapphire Rapids, memory-optimized)", "aws", "Sapphire Rapids", True, aws("r7i", M7_SIZES), []),
    ("m7a (AMD Genoa, EPYC 9R14)", "aws", "EPYC 4th Gen", False, aws("m7a", M7_SIZES), []),
    ("c7a (Genoa, compute-optimized)", "aws", "EPYC 4th Gen", False, aws("c7a", M7_SIZES), []),
    ("r7a (Genoa, memory-optimized)", "aws", "EPYC 4th Gen", True, aws("r7a", M7_SIZES), []),
    ("m6a (AMD Milan, EPYC 7R13)", "aws", "EPYC 3rd Gen", False, aws("m6a", M7_SIZES), []),
    ("c6a (Milan, compute-optimized)", "aws", "EPYC 3rd Gen", False, aws("c6a", M7_SIZES), []),
    ("m7g (Graviton 3, Neoverse V1)", "aws", "Graviton3", False, aws("m7g", M7_SIZES),
     ["CCF has no measured SPECpower for Graviton silicon (EPYC 2nd Gen proxy)."]),
    ("c7g (Graviton 3, compute-optimized)", "aws", "Graviton3", False, aws("c7g", M7_SIZES), []),
    ("m8g (Graviton 4, Neoverse V2)", "aws", "Graviton4", False, aws("m8g", M7_SIZES), []),
    ("c8g (Graviton 4, compute-optimized)", "aws", "Graviton4", False, aws("c8g", M7_SIZES), []),
    ("m8a (AMD Turin, EPYC 5th Gen, Genoa proxy)", "aws", "EPYC 4th Gen", False, aws("m8a", M7_SIZES),
     ["Turin is proxied to Genoa pending an upstream CCF correction,",
      "see table.rs module docs and docs/LIMITATIONS.md."]),
    ("c8a (Turin, compute-optimized, Genoa proxy)", "aws", "EPYC 4th Gen", False, aws("c8a", M7_SIZES), []),
    ("m8i (Intel Emerald Rapids, general purpose)", "aws", "Emerald Rapids", False, aws("m8i", M7_SIZES), []),
    ("c8i (Emerald Rapids, compute-optimized)", "aws", "Emerald Rapids", False, aws("c8i", M7_SIZES), []),
    ("n2-standard (Cascade Lake, general purpose)", "gcp", "Cascade Lake", False,
     gcp("n2-standard", [2, 4, 8, 16, 32, 48, 64, 80, 96, 128]), []),
    ("n2-highcpu (Cascade Lake, compute-optimized)", "gcp", "Cascade Lake", False,
     gcp("n2-highcpu", [2, 4, 8, 16, 32, 48, 64, 80, 96]), []),
    ("n2-highmem (Cascade Lake, memory-optimized)", "gcp", "Cascade Lake", True,
     gcp("n2-highmem", [2, 4, 8, 16, 32, 48, 64, 80, 96, 128]), []),
    ("e2-standard (EPYC 2nd Gen / Skylake mix, general purpose)", "gcp", "EPYC 2nd Gen", False,
     gcp("e2-standard", [2, 4, 8, 16, 32]), []),
    ("c2-standard (Cascade Lake, compute-optimized)", "gcp", "Cascade Lake", False,
     gcp("c2-standard", [4, 8, 16, 30, 60]), []),
    ("c3 (Sapphire Rapids, general purpose)", "gcp", "Sapphire Rapids", False,
     gcp("c3-standard", [4, 8, 22, 44, 88, 176]), []),
    ("c3d (AMD Genoa, EPYC 9004)", "gcp", "EPYC 4th Gen", False,
     gcp("c3d-standard", [4, 8, 16, 30, 60, 180]), []),
    ("c4 (Emerald Rapids, Xeon Platinum 8592+)", "gcp", "Emerald Rapids", False,
     gcp("c4-standard", [2, 4, 8, 16, 32, 96]), []),
    ("n2d (Genoa-era newer, EPYC 9004)", "gcp", "EPYC 4th Gen", False,
     gcp("n2d-standard", [2, 4, 8, 16, 32, 64]), []),
    ("c4a (Google Axion, Neoverse V2 ARM)", "aws", "Graviton4", False,
     gcp("c4a-standard", [1, 2, 4, 8, 16, 32, 48, 72]),
     ["No native ARM entry in the GCP CSV. Proxied to AWS Graviton 4",
      "(Neoverse V2 silicon family), itself an EPYC 2nd Gen placeholder."]),
    ("Standard_D v3 (Broadwell / Skylake)", "azure", "Skylake", False,
     azure("Standard_D{}s_v3", [2, 4, 8, 16, 32, 48, 64]), []),
    ("Standard_D v4 (Cascade Lake)", "azure", "Cascade Lake", False,
     azure("Standard_D{}s_v4", [2, 4, 8, 16, 32, 48, 64]), []),
    ("Standard_D v5 (Cascade Lake / Ice Lake)", "azure", "Cascade Lake", False,
     azure("Standard_D{}s_v5", [2, 4, 8, 16, 32, 48, 64, 96]), []),
    ("Standard_Das v5 (EPYC 3rd Gen)", "azure", "EPYC 3rd Gen", False,
     azure("Standard_D{}as_v5", [2, 4, 8, 16, 32, 48, 64, 96]), []),
    ("Standard_E v3 (Skylake, memory-optimized)", "azure", "Skylake", True,
     azure("Standard_E{}s_v3", [2, 4, 8, 16, 32, 48, 64]), []),
    ("Standard_E v4 (Cascade Lake, memory-optimized)", "azure", "Cascade Lake", True,
     azure("Standard_E{}s_v4", [2, 4, 8, 16, 32, 48, 64]), []),
    ("Standard_E v5 (Cascade Lake / Ice Lake, memory-optimized)", "azure", "Cascade Lake", True,
     azure("Standard_E{}s_v5", [2, 4, 8, 16, 32, 48, 64, 96]), []),
    ("Standard_F v2 (Cascade Lake, compute-optimized)", "azure", "Cascade Lake", False,
     azure("Standard_F{}s_v2", [2, 4, 8, 16, 32, 48, 64, 72]), []),
]

PROVIDER_BANNERS = {
    "t3 (Nitro, Cascade Lake, burstable)":
        "AWS instances (vCPU * per_vCPU_coefficient from CCF {date}\n"
        "coefficients-aws-use.csv; no baseboard overhead column)",
    "n2-standard (Cascade Lake, general purpose)":
        "GCP instances (vCPU * per_vCPU_coefficient from CCF {date}\n"
        "coefficients-gcp-use.csv)",
    "Standard_D v3 (Broadwell / Skylake)":
        "Azure instances (vCPU * per_vCPU_coefficient from CCF {date}\n"
        "coefficients-azure-use.csv). Families absent from this CSV live\n"
        "in `MANUAL_INSTANCE_ROWS` (table.rs)",
}

# Architectures whose appearance in a fresh CSV means a manual row in
# table.rs (or a cross-provider proxy in FAMILIES, like c4a) is now
# covered upstream and should be dropped or re-pointed.
WATCHLIST = [
    ("gcp", "EPYC 5th Gen", "c4d rows (GCP Turin)"),
    ("gcp", "Altra", "t2a rows (Ampere Altra)"),
    ("gcp", "Ampere", "t2a rows (Ampere Altra)"),
    ("gcp", "Axion", "c4a proxy (currently AWS Graviton4)"),
    ("gcp", "Neoverse", "c4a proxy (currently AWS Graviton4)"),
    ("azure", "Emerald Rapids", "Standard_D*_v6 / Standard_E*_v6 rows"),
    ("azure", "EPYC 4th Gen", "Standard_Dads_v6 rows"),
    ("azure", "Cobalt", "Standard_Dps_v6 rows (Cobalt 100)"),
]

HEADER = """\
// GENERATED FILE - DO NOT EDIT BY HAND.
// Regenerate with: python3 scripts/refresh-instance-power.py
//
// `(instance_type, idle_watts, max_watts)` rows derived from the CCF
// per-architecture coefficients (`coefficients-{aws,gcp,azure}-use.csv`),
// snapshot {date}:
// https://github.com/{repo}/tree/{sha}
//
// idle_watts = vCPU * idle_per_vcpu_coefficient (same for max).
// Memory-optimized families add the DRAM premium (+0.16/+0.40 per
// vCPU). Entries absent from the CCF CSVs live in
// `MANUAL_INSTANCE_ROWS` (table.rs). Full methodology in `table.rs`.

/// Surfaced in disclosure reports via `embedded_specpower_vintage` and
/// grep-audited by release procedure step 2.5. Stamped with the
/// ccf-coefficients HEAD commit date by the refresh script.
pub(crate) const SPECPOWER_VINTAGE: &str = "{date} (CCF aligned)";

pub(super) static GENERATED_INSTANCE_ROWS: &[(&str, f64, f64)] = &[
"""


def request(url: str) -> urllib.request.Request:
    headers = {"User-Agent": "perf-sentinel-refresh"}
    # Unauthenticated api.github.com calls from CI runner IPs hit the
    # shared 60/hr rate limit; use the workflow token when present.
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"
    return urllib.request.Request(url, headers=headers)


def http_json(url: str) -> dict:
    with urllib.request.urlopen(request(url), timeout=60) as resp:
        data = json.load(resp)
    return data


def fetch_ccf(ref: str) -> "tuple[dict[tuple[str, str], tuple[float, float]], str, str]":
    # Resolve the last commit touching the coefficient CSVs, not the
    # repo HEAD: unrelated upstream commits (README, CI) must not bump
    # the vintage or dirty the generated file.
    commits = http_json(
        f"https://api.github.com/repos/{CCF_REPO}/commits?sha={ref}&path=output&per_page=1"
    )
    if not commits:
        raise ValueError(f"no commit touching output/ found at ref {ref}")
    sha = commits[0]["sha"][:12]
    date = commits[0]["commit"]["committer"]["date"][:10]
    coefficients: "dict[tuple[str, str], tuple[float, float]]" = {}
    for provider in ("aws", "gcp", "azure"):
        url = (
            f"https://raw.githubusercontent.com/{CCF_REPO}/{sha}"
            f"/output/coefficients-{provider}-use.csv"
        )
        with urllib.request.urlopen(request(url), timeout=60) as resp:
            text = io.TextIOWrapper(resp, encoding="utf-8")
            for row in csv.DictReader(text):
                key = (provider, row["Architecture"])
                coefficients[key] = (float(row["Min Watts"]), float(row["Max Watts"]))
    if not coefficients:
        raise ValueError("no coefficient rows parsed, CCF format changed?")
    return coefficients, sha, date


def parse_existing(text: str) -> "dict[str, tuple[float, float]]":
    return {
        name: (float(idle), float(mx))
        for name, idle, mx in re.findall(r'\("([^"]+)", ([0-9.]+), ([0-9.]+)\),', text)
    }


def render(coefficients: dict, sha: str, date: str) -> str:
    lines = []
    for label, provider, arch, dram, instances, notes in FAMILIES:
        if label in PROVIDER_BANNERS:
            banner = PROVIDER_BANNERS[label].format(date=date)
            lines.append("    // " + "=" * 64)
            lines.extend(f"    // {b}" for b in banner.split("\n"))
            lines.append("    // " + "=" * 64)
        if (provider, arch) not in coefficients:
            raise ValueError(f"architecture '{arch}' missing from {provider} CSV (family {label})")
        idle_c, max_c = coefficients[(provider, arch)]
        lines.append(f"    // --- {label} ---")
        dram_note = " + DRAM premium (+0.16/+0.40)" if dram else ""
        lines.append(f"    // CCF {provider} {arch}: {idle_c:.3f} idle / {max_c:.3f} max W/vCPU{dram_note}.")
        lines.extend(f"    // {n}" for n in notes)
        for name, vcpu in instances:
            idle = round(vcpu * (idle_c + (DRAM_IDLE if dram else 0.0)), 1)
            mx = round(vcpu * (max_c + (DRAM_MAX if dram else 0.0)), 1)
            lines.append(f'    ("{name}", {idle}, {mx}),')
    return (
        HEADER.replace("{repo}", CCF_REPO).replace("{sha}", sha).replace("{date}", date)
        + "\n".join(lines) + "\n];\n"
    )


def main() -> None:
    ref = sys.argv[1] if len(sys.argv) > 1 else "main"
    coefficients, sha, date = fetch_ccf(ref)
    old_text = TARGET.read_text(encoding="utf-8")
    old = parse_existing(old_text)
    new_text = render(coefficients, sha, date)
    new = parse_existing(new_text)

    changed = 0
    for name, (idle, mx) in new.items():
        old_row = old.get(name)
        if old_row != (idle, mx):
            changed += 1
            was = f"{old_row[0]}/{old_row[1]}" if old_row else "new"
            print(f"{name:<22} {was:>14} -> {idle}/{mx}")
    for name in old:
        if name not in new:
            print(f"warning: {name} removed from generated rows", file=sys.stderr)
    print(f"{changed} of {len(new)} generated rows changed")

    old_vintage = re.search(r'SPECPOWER_VINTAGE: &str = "([^"]+)"', old_text)
    if old_vintage and not old_vintage.group(1).startswith(date):
        print(
            f"warning: SPECPOWER_VINTAGE bumps to '{date}'. Operator TOMLs "
            "pinning specpower_table_version must move to that token for "
            "Official reports (see report/periodic/validator.rs).",
            file=sys.stderr,
        )
    turin = coefficients.get(("aws", "EPYC 5th Gen"))
    genoa = coefficients.get(("aws", "EPYC 4th Gen"))
    if turin and genoa and turin[0] < 2.0 * genoa[0]:
        print(
            "warning: CCF EPYC 5th Gen coefficient normalized. Re-evaluate "
            "the Turin->Genoa proxy (docs/LIMITATIONS.md).",
            file=sys.stderr,
        )
    for provider, needle, rows in WATCHLIST:
        for csv_provider, arch in coefficients:
            if csv_provider == provider and needle in arch:
                print(
                    f"warning: {provider} CSV now covers '{arch}'. The manual "
                    f"{rows} in table.rs can move to the generated manifest.",
                    file=sys.stderr,
                )

    TARGET.write_text(new_text, encoding="utf-8", newline="\n")
    print(f"wrote {TARGET.relative_to(REPO_ROOT)}")


if __name__ == "__main__":
    main()
