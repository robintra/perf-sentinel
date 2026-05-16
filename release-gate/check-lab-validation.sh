#!/usr/bin/env bash
# Release-gate pre-flight: assert a recent PASS lab validation exists for
# a perf-sentinel version before tagging it for public release.
#
# Reads a tab-separated `lab-validations.txt` ledger. One line per
# validation:
#   <version>\t<lab_commit_sha>\t<YYYY-MM-DD>\t<PASS|FAIL>
#
# Lines starting with `#` and empty lines are ignored. Lines with a
# field count other than 4 are skipped with a warning on stderr.
#
# Exit codes:
#   0 - gate passed, version has a fresh PASS entry
#   1 - gate failed (ledger missing, no PASS entry, stale, or unparseable date)
#   2 - usage error (missing or malformed CLI argument)

set -euo pipefail
# Locale-pin for deterministic sort/awk/date behavior regardless of caller env.
export LC_ALL=C

VERSION=""
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LEDGER="${LEDGER:-${SCRIPT_DIR}/lab-validations.txt}"
MAX_AGE_DAYS=30

usage() {
  cat <<EOF
Usage: $(basename "$0") --version vX.Y.Z [--ledger PATH] [--max-age-days N]

Asserts that a PASS lab validation exists for the requested version in
the ledger, dated within the last N days. Defaults: ledger next to this
script (release-gate/lab-validations.txt), max-age-days=30.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)      VERSION="${2:?--version requires an argument}"; shift 2 ;;
    --ledger)       LEDGER="${2:?--ledger requires an argument}"; shift 2 ;;
    --max-age-days) MAX_AGE_DAYS="${2:?--max-age-days requires an argument}"; shift 2 ;;
    -h|--help)      usage; exit 0 ;;
    *)              echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[ -n "${VERSION}" ] || { echo "missing --version" >&2; usage >&2; exit 2; }

# Validate inputs. `MAX_AGE_DAYS` flows into bash arithmetic (bounded to
# 5 digits to keep `cutoff_epoch` well inside signed int64). `VERSION`
# accepts both vX.Y.Z and X.Y.Z (the latter normalized to vX.Y.Z), to
# stay aligned with scripts/check-tag-version.sh which accepts both.
[[ "${MAX_AGE_DAYS}" =~ ^[1-9][0-9]{0,4}$ ]] || { echo "release-gate: --max-age-days must be a positive integer in [1, 99999] (got '${MAX_AGE_DAYS}')." >&2; exit 2; }
[[ "${VERSION}" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || { echo "release-gate: --version must match vX.Y.Z or X.Y.Z (optionally with -suffix), got '${VERSION}'." >&2; exit 2; }
[[ "${VERSION}" == v* ]] || VERSION="v${VERSION}"

if [ ! -f "${LEDGER}" ]; then
  echo "release-gate: ledger ${LEDGER} not found." >&2
  echo "Run a lab validation in the perf-sentinel-simulation-lab repo, then use its scripts/record-validation.sh to produce a line to append here." >&2
  exit 1
fi

# Distinguish "ledger empty / comments-only" from "no PASS entry": the
# remedy differs (bootstrap a first validation vs run the lab on the
# target version).
data_line_count="$(awk '!/^#/ && !/^[[:space:]]*$/ { n++ } END { print n+0 }' "${LEDGER}")"
if [ "${data_line_count}" -eq 0 ]; then
  echo "release-gate: ledger ${LEDGER} contains no validation entries (only comments or blank lines)." >&2
  echo "Bootstrap it by running a lab validation, then appending the line from scripts/record-validation.sh." >&2
  exit 1
fi

# Portable "today minus N days" in seconds-since-epoch. Supports BSD and GNU date.
today_epoch="$(date -u +%s)"
[[ "${today_epoch}" =~ ^[0-9]+$ ]] || { echo "release-gate: cannot read current time as epoch (got '${today_epoch}')." >&2; exit 1; }
cutoff_epoch=$(( today_epoch - MAX_AGE_DAYS * 86400 ))

# Find the most recent PASS line for the requested version. Strips a
# trailing CR (CRLF input) before comparing. Warns on malformed lines.
match="$(awk -F '\t' -v ver="${VERSION}" '
  BEGIN { OFS = "\t" }
  /^#/ { next }
  /^[[:space:]]*$/ { next }
  { sub(/\r$/, "", $NF); $1 = $1 }
  NF != 4 { printf "release-gate: warning: ignoring malformed line %d (expected 4 tab-separated fields, got %d)\n", NR, NF | "cat 1>&2"; next }
  $1 == ver && $4 == "PASS" { print $0 }
' "${LEDGER}")"

if [ -z "${match}" ]; then
  echo "release-gate: no PASS entry for ${VERSION} in ${LEDGER}." >&2
  echo "Run the lab against ${VERSION}, append a PASS line via scripts/record-validation.sh, then retry." >&2
  exit 1
fi

# Keep the latest by date (column 3) with a deterministic tie-break on
# the lab commit sha (column 2). Lexicographic sort is correct for
# YYYY-MM-DD.
latest_line="$(printf '%s\n' "${match}" | sort -s -t$'\t' -k3,3 -k2,2 | tail -1)"
# Tab-delimited read so an embedded space in column 2 cannot truncate latest_sha.
IFS=$'\t' read -r latest_date latest_sha < <(printf '%s' "${latest_line}" | awk -F '\t' 'BEGIN{OFS="\t"} {print $3, $2}')

# Defensive: regex-validate both columns the gate exposes downstream.
# Date format gates date(1) below, sha format gates the operator-facing
# message (and forces ledger producers to stay on schema).
[[ "${latest_date}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || { echo "release-gate: invalid date format '${latest_date}' in ledger (expected YYYY-MM-DD)." >&2; exit 1; }
[[ "${latest_sha}" =~ ^[0-9a-f]{7,64}$ ]] || { echo "release-gate: invalid lab commit sha '${latest_sha}' in ledger (expected 7-64 hex chars)." >&2; exit 1; }

# Convert YYYY-MM-DD to epoch (UTC midnight). BSD date uses -j -f, GNU uses -d.
if latest_epoch="$(date -u -j -f "%Y-%m-%d" "${latest_date}" +%s 2>/dev/null)"; then
  :
elif latest_epoch="$(date -u -d "${latest_date}" +%s 2>/dev/null)"; then
  :
else
  echo "release-gate: cannot parse date '${latest_date}' from ledger." >&2
  exit 1
fi
[[ "${latest_epoch}" =~ ^[0-9]+$ ]] || { echo "release-gate: epoch conversion of '${latest_date}' returned non-integer '${latest_epoch}'." >&2; exit 1; }

# Reject future-dated entries. Most likely cause is an operator typo
# (e.g. 2099 instead of 2026) rather than an attack, but the gate must
# not silently approve a "PASS" claimed for a future date.
if [ "${latest_epoch}" -gt "${today_epoch}" ]; then
  echo "release-gate: ledger entry for ${VERSION} is dated ${latest_date}, which is in the future (today is $(date -u +%F)). Refusing to trust." >&2
  exit 1
fi

if [ "${latest_epoch}" -lt "${cutoff_epoch}" ]; then
  age_days=$(( (today_epoch - latest_epoch) / 86400 ))
  echo "release-gate: latest PASS for ${VERSION} is ${age_days} days old (lab commit ${latest_sha}, ${latest_date})." >&2
  echo "Threshold is ${MAX_AGE_DAYS} days. Re-run the lab against ${VERSION} and record a fresh entry." >&2
  exit 1
fi

age_days=$(( (today_epoch - latest_epoch) / 86400 ))
echo "release-gate: PASS for ${VERSION} dated ${latest_date} (lab commit ${latest_sha}, ${age_days}d old, threshold ${MAX_AGE_DAYS}d). OK to release."
exit 0
