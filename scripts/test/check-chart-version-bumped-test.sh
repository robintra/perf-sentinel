#!/usr/bin/env bash
#
# Self-test harness for scripts/check-chart-version-bumped.sh.
#
# Usage:
#   scripts/test/check-chart-version-bumped-test.sh
#
# Builds a throwaway git repo under $(mktemp -d) for each scenario,
# seeds base and head states, invokes the script under test with
# GITHUB_BASE_REF=main, and asserts the expected exit code. Prints
# one PASS/FAIL line per scenario and a summary. Exit 0 iff every
# scenario matches.

set -u

SCRIPT_UNDER_TEST="$(cd "$(dirname "$0")/.." && pwd)/check-chart-version-bumped.sh"
CHART_DIR="charts/perf-sentinel"
CHART_FILE="${CHART_DIR}/Chart.yaml"
CHART_CHANGELOG="${CHART_DIR}/CHANGELOG.md"

PASS=0
FAIL=0

# Write a minimal Chart.yaml with the given version.
write_chart() {
  cat > "${CHART_FILE}" <<EOF
apiVersion: v2
name: perf-sentinel
version: $1
appVersion: "0.5.1"
EOF
}

# Write a CHANGELOG with the given sections (one per arg, most recent first).
write_changelog() {
  {
    echo "# Changelog"
    echo ""
    for v in "$@"; do
      echo "## [${v}]"
      echo ""
      echo "entry for ${v}."
      echo ""
    done
  } > "${CHART_CHANGELOG}"
}

# Commit everything currently staged/unstaged with the given message.
commit_all() {
  git add -A
  git commit -q -m "$1"
}

# Run one scenario in a throwaway repo and assert exit code.
run_scenario() {
  local name="$1"
  local expected="$2"
  local setup_func="$3"

  local tmpdir
  tmpdir=$(mktemp -d)
  local actual
  actual=$(
    cd "${tmpdir}" || exit 1
    git init -q -b main
    git config user.email "test@example.com"
    git config user.name "test-runner"
    mkdir -p "${CHART_DIR}"

    "${setup_func}"

    GITHUB_BASE_REF=main "${SCRIPT_UNDER_TEST}" > /dev/null 2>&1
    echo "$?"
  )

  if [ "${actual}" = "${expected}" ]; then
    echo "PASS: ${name} (exit ${actual})"
    PASS=$((PASS + 1))
  else
    echo "FAIL: ${name} (expected ${expected}, got ${actual})"
    FAIL=$((FAIL + 1))
  fi
  rm -rf "${tmpdir}"
}

# --- Scenario setups ---------------------------------------------------------

# No chart change between base and head.
scenario_no_change() {
  write_chart "0.1.0"
  write_changelog "0.1.0"
  commit_all "base"
  git update-ref refs/remotes/origin/main HEAD
  # Touch an unrelated file on head.
  echo "unrelated" > README.md
  commit_all "head, no chart change"
}

# Only CHANGELOG changed (allowed without version bump).
scenario_changelog_only() {
  write_chart "0.1.0"
  write_changelog "0.1.0"
  commit_all "base"
  git update-ref refs/remotes/origin/main HEAD
  write_changelog "0.1.0"  # same version, rewrite body
  echo "### Added" >> "${CHART_CHANGELOG}"
  echo "- preview." >> "${CHART_CHANGELOG}"
  commit_all "head, changelog only"
}

# Chart changed but no version bump.
scenario_unbumped() {
  write_chart "0.1.0"
  write_changelog "0.1.0"
  commit_all "base"
  git update-ref refs/remotes/origin/main HEAD
  echo "# test change" >> "${CHART_DIR}/values.yaml"
  commit_all "head, chart change no bump"
}

# Chart change, version bumped, matching changelog section present on head only.
scenario_proper_bump() {
  write_chart "0.1.0"
  write_changelog "0.1.0"
  commit_all "base"
  git update-ref refs/remotes/origin/main HEAD
  write_chart "0.1.1"
  write_changelog "0.1.1" "0.1.0"
  echo "# test change" >> "${CHART_DIR}/values.yaml"
  commit_all "head, proper bump"
}

# Chart change, version bumped, but NO matching ## [0.1.1] section on head.
scenario_missing_section() {
  write_chart "0.1.0"
  write_changelog "0.1.0"
  commit_all "base"
  git update-ref refs/remotes/origin/main HEAD
  write_chart "0.1.1"
  # changelog still only has [0.1.0]
  echo "# test change" >> "${CHART_DIR}/values.yaml"
  commit_all "head, bump without changelog section"
}

# Chart change, version bumped, section was already on base (not new on head).
scenario_section_on_base() {
  write_chart "0.1.0"
  write_changelog "0.1.1" "0.1.0"  # both sections already on base
  commit_all "base"
  git update-ref refs/remotes/origin/main HEAD
  write_chart "0.1.1"
  echo "# test change" >> "${CHART_DIR}/values.yaml"
  commit_all "head, bump but section was on base"
}

# Version downgrade.
scenario_downgrade() {
  write_chart "0.2.0"
  write_changelog "0.2.0"
  commit_all "base"
  git update-ref refs/remotes/origin/main HEAD
  write_chart "0.1.9"
  write_changelog "0.1.9" "0.2.0"
  echo "# test change" >> "${CHART_DIR}/values.yaml"
  commit_all "head, downgrade"
}

# Bump with a header but empty section body (header + blank then next header).
scenario_empty_section_body() {
  write_chart "0.1.0"
  write_changelog "0.1.0"
  commit_all "base"
  git update-ref refs/remotes/origin/main HEAD
  write_chart "0.1.1"
  {
    echo "# Changelog"
    echo ""
    echo "## [0.1.1]"
    echo ""
    echo "## [0.1.0]"
    echo ""
    echo "entry for 0.1.0."
    echo ""
  } > "${CHART_CHANGELOG}"
  echo "# test change" >> "${CHART_DIR}/values.yaml"
  commit_all "head, bump with empty section"
}

# --- Drive ------------------------------------------------------------------

run_scenario "no chart change"                      0 scenario_no_change
run_scenario "CHANGELOG-only change"                0 scenario_changelog_only
run_scenario "chart change without version bump"    1 scenario_unbumped
run_scenario "proper bump with new section"         0 scenario_proper_bump
run_scenario "bump without CHANGELOG section"       1 scenario_missing_section
run_scenario "bump but section was on base"         1 scenario_section_on_base
run_scenario "version downgrade"                    1 scenario_downgrade
run_scenario "bump with empty section body"         1 scenario_empty_section_body

echo ""
echo "Summary: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" -eq 0 ]
