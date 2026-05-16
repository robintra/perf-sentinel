#!/usr/bin/env bash
#
# Self-test harness for release-gate/check-lab-validation.sh.
#
# Usage:
#   scripts/test/check-lab-validation-test.sh
#
# For each scenario: builds a throwaway ledger fixture under $(mktemp -d),
# invokes the script under test with scenario-specific CLI args, and
# asserts the expected exit code. Prints one PASS/FAIL line per scenario
# and a summary. Exit 0 iff every scenario matches.

set -u

SCRIPT_UNDER_TEST="$(cd "$(dirname "$0")/../.." && pwd)/release-gate/check-lab-validation.sh"

PASS=0
FAIL=0
TEST_ARGS=()

# Portable "today" and "today minus 2 days" in YYYY-MM-DD (BSD + GNU).
TODAY_ISO="$(date -u +%F)"
if TWO_DAYS_AGO_ISO="$(date -u -v-2d +%F 2>/dev/null)"; then
  :
elif TWO_DAYS_AGO_ISO="$(date -u -d '2 days ago' +%F 2>/dev/null)"; then
  :
else
  echo "harness error: cannot compute 'today - 2 days' on this date(1) flavor" >&2
  exit 1
fi

# Append one tab-separated ledger line (LF-terminated).
write_line() {
  local ledger="$1" ver="$2" sha="$3" date_iso="$4" verdict="$5"
  printf '%s\t%s\t%s\t%s\n' "${ver}" "${sha}" "${date_iso}" "${verdict}" >> "${ledger}"
}

# Append one tab-separated ledger line with CRLF terminator.
write_line_crlf() {
  local ledger="$1" ver="$2" sha="$3" date_iso="$4" verdict="$5"
  printf '%s\t%s\t%s\t%s\r\n' "${ver}" "${sha}" "${date_iso}" "${verdict}" >> "${ledger}"
}

# Run one scenario in a throwaway dir and assert exit code. If a 4th
# argument is given, also assert that the gate's stdout contains that
# substring (useful when the exit code alone does not pin the behavior
# being tested, e.g. tie-break selection).
run_scenario() {
  local name="$1"
  local expected_exit="$2"
  local setup_func="$3"
  local expected_substr="${4:-}"

  local tmpdir
  tmpdir=$(mktemp -d) || { echo "harness error: mktemp -d failed" >&2; exit 1; }
  local actual_exit
  actual_exit=$(
    cd "${tmpdir}" || exit 1
    TEST_ARGS=()
    "${setup_func}"
    "${SCRIPT_UNDER_TEST}" "${TEST_ARGS[@]}" > stdout.txt 2>/dev/null
    echo "$?"
  )

  local actual_out=""
  if [ -f "${tmpdir}/stdout.txt" ]; then
    actual_out=$(cat "${tmpdir}/stdout.txt")
  fi

  local exit_ok=1
  local substr_ok=1
  [ "${actual_exit}" = "${expected_exit}" ] || exit_ok=0
  if [ -n "${expected_substr}" ]; then
    printf '%s' "${actual_out}" | grep -qF -- "${expected_substr}" || substr_ok=0
  fi

  if [ "${exit_ok}" = 1 ] && [ "${substr_ok}" = 1 ]; then
    echo "PASS: ${name} (exit ${actual_exit})"
    PASS=$((PASS + 1))
  else
    local detail=""
    if [ "${exit_ok}" = 0 ]; then
      detail="expected exit ${expected_exit}, got ${actual_exit}"
    fi
    if [ "${substr_ok}" = 0 ]; then
      [ -n "${detail}" ] && detail="${detail}; "
      detail="${detail}stdout missing substring '${expected_substr}'"
    fi
    echo "FAIL: ${name} (${detail})"
    FAIL=$((FAIL + 1))
  fi
  rm -rf "${tmpdir}"
}

# --- Scenario setups --------------------------------------------------------

scenario_ledger_absent() {
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/does-not-exist.txt")
}

scenario_ledger_zero_byte() {
  : > ledger.txt
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_ledger_comments_only() {
  printf '%s\n' '# header only' '# nothing else' '' > ledger.txt
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_pass_fresh_today() {
  write_line ledger.txt v0.1.0 abc1234 "${TODAY_ISO}" PASS
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_pass_stale() {
  write_line ledger.txt v0.1.0 abc1234 "${TWO_DAYS_AGO_ISO}" PASS
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt" --max-age-days 1)
}

scenario_fail_only() {
  write_line ledger.txt v0.1.0 abc1234 "${TODAY_ISO}" FAIL
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_version_no_v_prefix() {
  write_line ledger.txt v0.1.0 abc1234 "${TODAY_ISO}" PASS
  TEST_ARGS=(--version 0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_max_age_zero_rejected() {
  TEST_ARGS=(--version v0.1.0 --max-age-days 0 --ledger "${PWD}/dummy.txt")
}

scenario_max_age_non_numeric_rejected() {
  TEST_ARGS=(--version v0.1.0 --max-age-days abc --ledger "${PWD}/dummy.txt")
}

scenario_max_age_too_large_rejected() {
  TEST_ARGS=(--version v0.1.0 --max-age-days 999999999 --ledger "${PWD}/dummy.txt")
}

scenario_crlf_line() {
  write_line_crlf ledger.txt v0.1.0 abc1234 "${TODAY_ISO}" PASS
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_malformed_line_skipped() {
  # 3-column malformed line first, then a valid 4-column line.
  printf 'v0.1.0\tonly-three-cols\tPASS\n' > ledger.txt
  write_line ledger.txt v0.1.0 abc1234 "${TODAY_ISO}" PASS
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_duplicate_tie_break() {
  # Same version, same date, two different shas. Gate must exit 0
  # deterministically and pick `fffffff` (sort -k3,3 -k2,2 chooses the
  # lexicographically larger sha on the tie). The success-line stdout
  # substring assertion in the driver pins this behavior.
  write_line ledger.txt v0.1.0 aaaaaaa "${TODAY_ISO}" PASS
  write_line ledger.txt v0.1.0 fffffff "${TODAY_ISO}" PASS
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_future_date_refused() {
  write_line ledger.txt v0.1.0 abc1234 '2099-12-31' PASS
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_forged_sha_with_space() {
  # Column 2 contains a literal space, breaking the sha hex regex.
  printf 'v0.1.0\tabc def\t%s\tPASS\n' "${TODAY_ISO}" > ledger.txt
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_forged_date_now() {
  # Fuzzy "now" string in column 3 must be rejected by the date regex
  # before being passed to date(1) (GNU date would accept it).
  printf 'v0.1.0\tabc1234\tnow\tPASS\n' > ledger.txt
  TEST_ARGS=(--version v0.1.0 --ledger "${PWD}/ledger.txt")
}

scenario_version_malformed_rejected() {
  # `--version foobar` does not match the vX.Y.Z regex, exits 2 before
  # any ledger read. No fixture needed.
  TEST_ARGS=(--version foobar --ledger "${PWD}/dummy.txt")
}

# --- Drive -----------------------------------------------------------------

run_scenario "ledger file absent"                       1 scenario_ledger_absent
run_scenario "ledger 0 byte"                            1 scenario_ledger_zero_byte
run_scenario "ledger comments-only"                     1 scenario_ledger_comments_only
run_scenario "PASS fresh today"                         0 scenario_pass_fresh_today
run_scenario "PASS stale (max-age 1, date -2d)"         1 scenario_pass_stale
run_scenario "FAIL only for version"                    1 scenario_fail_only
run_scenario "--version without v normalized"           0 scenario_version_no_v_prefix
run_scenario "--max-age-days 0 rejected"                2 scenario_max_age_zero_rejected
run_scenario "--max-age-days abc rejected"              2 scenario_max_age_non_numeric_rejected
run_scenario "--max-age-days 999999999 rejected"        2 scenario_max_age_too_large_rejected
run_scenario "CRLF line handled"                        0 scenario_crlf_line
run_scenario "malformed 3-col line skipped"             0 scenario_malformed_line_skipped
run_scenario "duplicate same-date tie-break"            0 scenario_duplicate_tie_break "lab commit fffffff"
run_scenario "future date refused"                      1 scenario_future_date_refused
run_scenario "forged sha with space rejected"           1 scenario_forged_sha_with_space
run_scenario "forged date 'now' rejected"               1 scenario_forged_date_now
run_scenario "--version foobar rejected"                2 scenario_version_malformed_rejected

echo ""
echo "Summary: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" -eq 0 ]
