#!/usr/bin/env bash
#
# Guard PRs that modify the Helm chart without bumping its version.
#
# Usage:
#   scripts/check-chart-version-bumped.sh                 # reads $GITHUB_BASE_REF
#   scripts/check-chart-version-bumped.sh <base-ref>      # explicit base ref
#
# Called by .github/workflows/helm-ci.yml for every PR touching
# `charts/**`. Also runnable locally before pushing a PR to catch the
# drift without waiting for CI.
#
# Behavior:
#   1. Resolve the base ref. Priority: explicit arg, then
#      $GITHUB_BASE_REF (populated in PR workflows).
#   2. Enumerate files changed under charts/perf-sentinel/ between
#      origin/<base-ref> and HEAD.
#   3. If nothing changed there, exit 0.
#   4. If only charts/perf-sentinel/CHANGELOG.md changed, exit 0
#      (chart changelog edits without a version bump are allowed).
#   5. Otherwise read the top-level `version:` field from
#      charts/perf-sentinel/Chart.yaml on HEAD and on origin/<base-ref>.
#   6. Exit 0 if they differ (a bump happened), 1 otherwise.
#
# Exit codes:
#   0 - no chart change, or a proper version bump
#   1 - unbumped chart change, missing base ref, or git / parse failure

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

# Extract the top-level `version:` field from a Chart.yaml content
# received on stdin. Awk independently, to keep the script standalone.
parse_version_from_stdin() {
  awk '
    /^version:[[:space:]]/ {
      v = $2
      gsub(/^["'\'']|["'\'']$/, "", v)
      print v
      exit
    }
  '
}

BASE_REF=""
if [ "$#" -ge 1 ] && [ -n "${1:-}" ]; then
  BASE_REF="$1"
elif [ -n "${GITHUB_BASE_REF:-}" ]; then
  BASE_REF="${GITHUB_BASE_REF}"
else
  emit_error "No base ref supplied. Pass it as arg 1 or set GITHUB_BASE_REF."
  exit 1
fi

CHART_DIR="charts/perf-sentinel"
CHART_FILE="${CHART_DIR}/Chart.yaml"
CHART_CHANGELOG="${CHART_DIR}/CHANGELOG.md"

# Enumerate changed files under the chart directory. Use `git diff`
# against the merge-base (the `...HEAD` syntax) so the comparison
# matches what GitHub shows on the PR.
if ! CHANGED=$(git diff --name-only "origin/${BASE_REF}...HEAD" -- "${CHART_DIR}" 2>/dev/null); then
  emit_error "git diff against origin/${BASE_REF} failed. Fetch the base branch first (actions/checkout needs fetch-depth: 0)."
  exit 1
fi

if [ -z "${CHANGED}" ]; then
  emit_notice "No files changed under ${CHART_DIR}. Skipping version-bump check."
  exit 0
fi

# Allow CHANGELOG-only edits without a version bump. Anything else in
# the chart directory counts as a change that requires a bump.
NON_CHANGELOG=$(printf '%s\n' "${CHANGED}" | grep -v "^${CHART_CHANGELOG}$" || true)
if [ -z "${NON_CHANGELOG}" ]; then
  emit_notice "Only ${CHART_CHANGELOG} changed. Skipping version-bump check."
  exit 0
fi

if [ ! -f "${CHART_FILE}" ]; then
  emit_error "Chart file not found at ${CHART_FILE}. Is the script running from the repository root?"
  exit 1
fi

HEAD_VERSION=$(parse_version_from_stdin < "${CHART_FILE}")
if [ -z "${HEAD_VERSION}" ]; then
  emit_error "Could not extract top-level version from ${CHART_FILE} on HEAD"
  exit 1
fi

if ! BASE_CHART=$(git show "origin/${BASE_REF}:${CHART_FILE}" 2>/dev/null); then
  emit_error "Could not read ${CHART_FILE} from origin/${BASE_REF}"
  exit 1
fi
BASE_VERSION=$(printf '%s\n' "${BASE_CHART}" | parse_version_from_stdin)
if [ -z "${BASE_VERSION}" ]; then
  emit_error "Could not extract top-level version from ${CHART_FILE} on origin/${BASE_REF}"
  exit 1
fi

if [ "${HEAD_VERSION}" != "${BASE_VERSION}" ]; then
  emit_notice "Chart version bumped: ${BASE_VERSION} -> ${HEAD_VERSION}"
  exit 0
fi

emit_error "Chart modified without a version bump. ${CHART_FILE} version is still ${HEAD_VERSION} on both HEAD and origin/${BASE_REF}. Changed files:"
printf '%s\n' "${NON_CHANGELOG}" | while IFS= read -r f; do
  emit_error "  ${f}"
done
emit_error "Bump ${CHART_FILE}:version (and add a matching section to ${CHART_CHANGELOG}) before merging."
exit 1
