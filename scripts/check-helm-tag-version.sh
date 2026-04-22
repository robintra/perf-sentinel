#!/usr/bin/env bash
#
# Verify a chart-release tag matches charts/perf-sentinel/Chart.yaml.
#
# Usage:
#   scripts/check-helm-tag-version.sh chart-v0.1.0
#   scripts/check-helm-tag-version.sh chart-v0.1.0-rc.1
#   scripts/check-helm-tag-version.sh 0.1.0             # leading `chart-v` optional
#
# Called by .github/workflows/helm-release.yml as the first gate of
# the chart publication flow, also runnable locally before tagging to
# catch drift without pushing.
#
# Behavior:
#   1. Strip the leading `chart-v` prefix from the argument if present.
#   2. Read the top-level `version:` field from
#      charts/perf-sentinel/Chart.yaml via awk (no yq dependency).
#   3. Fail loudly if the two disagree.
#
# Contract: the tag and Chart.yaml:version are compared with strict
# equality, including any prerelease suffix (e.g. `-rc.1`). Bump
# Chart.yaml:version to the full target (e.g. `0.1.0-rc.1`) before
# tagging.
#
# Exit codes:
#   0 - Chart.yaml:version matches the target version
#   1 - mismatch, missing Chart.yaml, malformed version line, or bad arg count

set -euo pipefail

emit_error() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*"
  else
    echo "error: $*" >&2
  fi
}
emit_notice() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::notice::$*"
  else
    echo "$*"
  fi
}

if [ "$#" -ne 1 ] || [ -z "${1:-}" ]; then
  echo "usage: $(basename "$0") <chart-tag-or-version>" >&2
  exit 1
fi

# Strip an optional leading `chart-v` so both `chart-v0.1.0-rc.1` and
# `0.1.0-rc.1` are accepted.
TARGET_VERSION="${1#chart-v}"

CHART_FILE="charts/perf-sentinel/Chart.yaml"
if [ ! -f "${CHART_FILE}" ]; then
  emit_error "Chart file not found at ${CHART_FILE}. Is the script running from the repository root?"
  exit 1
fi

# Extract the top-level `version:` field. Strips surrounding single or
# double quotes if present, matches the first occurrence to avoid any
# accidental hit on a deeper nested key.
CHART_VERSION=$(awk '
  /^version:[[:space:]]/ {
    v = $2
    gsub(/^["'\'']|["'\'']$/, "", v)
    print v
    exit
  }
' "${CHART_FILE}")

if [ -z "${CHART_VERSION}" ]; then
  emit_error "Could not extract top-level version from ${CHART_FILE}"
  exit 1
fi

if [ "${TARGET_VERSION}" != "${CHART_VERSION}" ]; then
  emit_error "Tag chart-v${TARGET_VERSION} does not match ${CHART_FILE} version (${CHART_VERSION}). Bump the chart version before tagging."
  exit 1
fi

emit_notice "Tag chart-v${TARGET_VERSION} matches ${CHART_FILE} version (${CHART_VERSION})"
