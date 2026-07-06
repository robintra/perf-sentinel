#!/usr/bin/env python3
"""Regenerate crates/sentinel-core/src/score/carbon_data.rs from Ember data.

Downloads the Ember yearly electricity release (CC-BY-4.0) and rewrites
the generated carbon intensity rows (annual gCO2/kWh, generation-based,
national granularity, latest year per country). Subnational rows stay in
MANUAL_CARBON_ROWS (carbon.rs) and are not touched.

Usage:
    python3 scripts/refresh-carbon-data.py [ember-csv-path-or-url]

Prints an old/new diff table on stdout and warnings on stderr. Exits 0
even when values changed (the CI workflow decides via git diff), 1 on
download or parse failure. Run `cargo fmt -p perf-sentinel-core`
afterwards (rustfmt owns the trailing-comment alignment; the CI
workflow does it). Run twice: the second run plus fmt must be a no-op.
"""

import csv
import io
import re
import sys
import urllib.request
from pathlib import Path

EMBER_URL = (
    "https://storage.googleapis.com/emb-prod-bkt-publicdata"
    "/public-downloads/yearly_full_release_long_format.csv"
)

REPO_ROOT = Path(__file__).resolve().parent.parent
TARGET = REPO_ROOT / "crates" / "sentinel-core" / "src" / "score" / "carbon_data.rs"

# (rust_key, iso3, trailing comment) per section. Adding a region means
# adding a line here, by hand. Subnational regions (North America,
# Brazil BR-CS) are deliberately absent: Ember is national-only and
# those rows live in MANUAL_CARBON_ROWS (carbon.rs).
SECTIONS: "list[tuple[str, str, list[tuple[str, str, str]]]]" = [
    ("AWS regions", "Aws", [
        ("eu-west-1", "IRL", "Ireland"),
        ("eu-west-2", "GBR", "London"),
        ("eu-west-3", "FRA", "Paris"),
        ("eu-central-1", "DEU", "Frankfurt"),
        ("eu-north-1", "SWE", "Stockholm"),
        ("ap-northeast-1", "JPN", "Tokyo"),
        ("ap-southeast-1", "SGP", "Singapore"),
        ("eu-west-4", "NLD", "Netherlands (canonical hourly key)"),
        ("eu-south-1", "ITA", "Milan (Italy)"),
        ("ap-southeast-2", "AUS", "Sydney"),
        ("ap-south-1", "IND", "Mumbai"),
    ]),
    ("GCP regions", "Gcp", [
        ("europe-west1", "BEL", "Belgium"),
        ("europe-west4", "NLD", "Netherlands"),
        ("europe-west9", "FRA", "Paris"),
        ("europe-north1", "FIN", "Finland"),
        ("europe-west8", "ITA", "Milan (Italy)"),
        ("europe-southwest1", "ESP", "Madrid (Spain)"),
        ("europe-central2", "POL", "Warsaw (Poland)"),
        ("europe-north2", "NOR", "Norway"),
        ("asia-northeast1", "JPN", "Tokyo"),
    ]),
    ("Azure regions", "Azure", [
        ("westeurope", "NLD", "Netherlands"),
        ("northeurope", "IRL", "Ireland"),
        ("francecentral", "FRA", ""),
        ("uksouth", "GBR", ""),
    ]),
    ("Country / ISO codes (generic PUE)", "Generic", [
        ("fr", "FRA", ""), ("de", "DEU", ""), ("gb", "GBR", ""),
        ("uk", "GBR", ""), ("us", "USA", ""), ("ie", "IRL", ""),
        ("se", "SWE", ""), ("no", "NOR", ""), ("jp", "JPN", ""),
        ("in", "IND", ""), ("au", "AUS", ""),
        ("sg", "SGP", ""), ("nl", "NLD", ""), ("be", "BEL", ""),
        ("fi", "FIN", ""), ("it", "ITA", ""), ("es", "ESP", ""),
        ("pl", "POL", ""),
    ]),
]

HEADER = """\
// GENERATED FILE - DO NOT EDIT BY HAND.
// Regenerate with: python3 scripts/refresh-carbon-data.py
//
// Carbon intensity rows (region_key, gCO2eq/kWh, provider) for regions
// on effectively national grids. Keys are lowercase. Subnational rows
// (North America, Brazil BR-CS) live in `MANUAL_CARBON_ROWS`
// (carbon.rs).
//
// Source: Ember yearly electricity data (CC-BY-4.0), generation-based
// annual gCO2/kWh, national granularity, latest year per country.
// https://ember-energy.org - methodology notes in docs/METHODOLOGY.md.

use super::carbon::Provider;

/// Grep-audited by release procedure step 2.5, like `PUE_VINTAGE`.
/// Stamped `ember-<latest-data-year>` by the refresh script.
#[allow(dead_code)]
pub(crate) const CARBON_TABLE_VINTAGE: &str = "{vintage}";

pub(super) static GENERATED_CARBON_ROWS: &[(&str, f64, Provider)] = &[
"""


def fetch_ember(source: str) -> "dict[str, tuple[float, int]]":
    """Latest CO2 intensity (gCO2/kWh) and its year, keyed by ISO3."""
    if source.startswith(("http://", "https://")):
        stream = io.TextIOWrapper(urllib.request.urlopen(source, timeout=120), encoding="utf-8")
    else:
        stream = open(source, encoding="utf-8", newline="")
    out: "dict[str, tuple[float, int]]" = {}
    with stream:
        for row in csv.DictReader(stream):
            if row["Variable"] != "CO2 intensity" or row["Unit"] != "gCO2/kWh":
                continue
            iso3, value = row["ISO 3 code"], row["Value"]
            if not iso3 or not value:
                continue
            year = int(row["Year"])
            if iso3 not in out or year > out[iso3][1]:
                out[iso3] = (float(value), year)
    if not out:
        raise ValueError("no 'CO2 intensity' rows found, Ember format changed?")
    return out


def parse_existing(text: str) -> "dict[str, float]":
    return {
        key: float(val)
        for key, val in re.findall(r'\("([^"]+)", ([0-9.]+), Provider::', text)
    }


def render(intensities: "dict[str, tuple[float, int]]") -> str:
    years = []
    lines = []
    for title, provider, rows in SECTIONS:
        lines.append(f"    // {title}")
        for key, iso3, comment in rows:
            if iso3 not in intensities:
                raise ValueError(f"{iso3} (for {key}) missing from Ember data")
            value, year = intensities[iso3]
            # No grid we embed sits outside (0, 2000] gCO2/kWh. A
            # corrupt upstream value must fail the run, not ship.
            if value <= 0.0 or value > 2000.0:
                raise ValueError(f"implausible intensity {value} for {iso3} ({key}, {year})")
            years.append(year)
            suffix = f" // {comment}" if comment else ""
            lines.append(f'    ("{key}", {round(value, 1)}, Provider::{provider}),{suffix}')
    vintage = f"ember-{max(years)}"
    return HEADER.replace("{vintage}", vintage) + "\n".join(lines) + "\n];\n"


def main() -> None:
    source = sys.argv[1] if len(sys.argv) > 1 else EMBER_URL
    intensities = fetch_ember(source)
    old = parse_existing(TARGET.read_text(encoding="utf-8"))
    new_text = render(intensities)
    new = parse_existing(new_text)

    print(f"{'key':<22} {'old':>8} {'new':>8} {'delta':>8}")
    for key, new_val in new.items():
        old_val = old.get(key)
        if old_val is None:
            print(f"{key:<22} {'-':>8} {new_val:>8} {'new':>8}")
            continue
        delta = (new_val - old_val) / old_val * 100.0
        marker = " <" if abs(delta) > 5.0 else ""
        print(f"{key:<22} {old_val:>8} {new_val:>8} {delta:>+7.1f}%{marker}")
        if abs(delta) > 5.0:
            print(
                f"warning: {key} moved {delta:+.1f}%. If it has an hourly "
                "profile, renormalize it in carbon_profiles.rs "
                "(hourly_profile_mean_* tests will fail on this PR).",
                file=sys.stderr,
            )
    for key in old:
        if key not in new:
            print(f"warning: {key} removed from generated rows", file=sys.stderr)

    TARGET.write_text(new_text, encoding="utf-8", newline="\n")
    print(f"wrote {TARGET.relative_to(REPO_ROOT)}")


if __name__ == "__main__":
    main()
