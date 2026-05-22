#!/usr/bin/env bash
#
# Self-test harness for scripts/release.sh.
#
# Usage:
#   scripts/test/release-test.sh
#
# Each scenario builds a throwaway git sandbox under $(mktemp -d) with
# a bare `origin` remote, a work tree that mirrors the perf-sentinel
# layout (Cargo.toml workspace + crates/foo + release-gate/ + scripts/
# release.sh copied in so it resolves REPO_ROOT to the sandbox), then
# invokes the sandbox copy of the script with scenario-specific args
# and asserts the expected exit code. Some scenarios additionally
# assert that a tag exists or does not exist locally or on the remote.

set -u

# SC2164: `cd "${tmpdir}/work"` after mktemp+git init, the path is
#         guaranteed by setup_sandbox, an exit-on-failure guard would
#         shadow real test failures.
# SC2016: EXTRA_ASSERT strings use deferred `$(...)` expansion via
#         `eval` in run_scenario, single quotes are intentional.
# shellcheck disable=SC2164,SC2016

REAL_REPO="$(cd "$(dirname "$0")/../.." && pwd)"
REAL_SCRIPT="${REAL_REPO}/scripts/release.sh"

PASS=0
FAIL=0
TEST_ARGS=()
TEST_SCRIPT=""
EXTRA_ASSERT=""

# Build a sandbox with a bare origin and a work tree pre-pushed.
# Caller must `cd "${tmpdir}/work"` afterwards and may then customize
# (dirty file, missing signingkey, etc.) before running the script.
setup_sandbox() {
  local tmpdir="$1"
  local version="$2"

  git init --bare "${tmpdir}/remote.git" -q
  # Override any operator-level core.hooksPath so the bare remote
  # honours its own hooks/ dir (needed for the push-failure scenario).
  git --git-dir="${tmpdir}/remote.git" config core.hooksPath "${tmpdir}/remote.git/hooks"

  # Ephemeral SSH key for tag signing: lets `git tag -s` traverse its
  # real signing path against a throwaway identity, no bypass needed
  # in the script under test.
  ssh-keygen -t ed25519 -f "${tmpdir}/sandbox.key" -N "" -q -C "release-test-sandbox"

  local work="${tmpdir}/work"
  git init "${work}" -q
  (
    cd "${work}" || exit 1
    git checkout -q -b main
    git config user.email test@example.com
    git config user.name test-runner
    git config gpg.format ssh
    git config user.signingkey "${tmpdir}/sandbox.key"
    # Disable any operator-level hooks (pre-commit linters, etc.) so
    # the sandbox setup commits are deterministic.
    git config core.hooksPath /dev/null
    git remote add origin "${tmpdir}/remote.git"

    cat > Cargo.toml <<EOF
[workspace]
members = ["crates/foo"]
resolver = "3"

[workspace.package]
version = "${version}"
edition = "2024"
EOF

    mkdir -p crates/foo/src
    cat > crates/foo/Cargo.toml <<'EOF'
[package]
name = "foo"
version.workspace = true
edition = "2024"
EOF
    touch crates/foo/src/lib.rs

    mkdir -p scripts release-gate
    cp "${REAL_REPO}/scripts/check-tag-version.sh" scripts/check-tag-version.sh
    cp "${REAL_REPO}/release-gate/check-lab-validation.sh" release-gate/check-lab-validation.sh
    cp "${REAL_SCRIPT}" scripts/release.sh
    chmod +x scripts/check-tag-version.sh release-gate/check-lab-validation.sh scripts/release.sh

    local today
    today="$(date -u +%F)"
    cat > release-gate/lab-validations.txt <<EOF
# Test ledger
v${version}	abc1234	${today}	PASS
EOF

    git add -A
    git commit -q -m "Initial sandbox"
    git push -q origin main
  )
}

# Driver: runs setup, invokes the script, captures exit + stdout +
# stderr, asserts the expected exit code and (optionally) a substring
# present in stderr, and runs EXTRA_ASSERT (a bash one-liner executed
# in the same tmpdir context) for state assertions on the sandbox.
run_scenario() {
  local name="$1"
  local expected_exit="$2"
  local setup_func="$3"
  local expected_stderr_substr="${4:-}"

  local tmpdir
  tmpdir=$(mktemp -d) || { echo "harness error: mktemp -d failed" >&2; exit 1; }
  # Interrupt-safe cleanup: survives Ctrl-C mid-scenario and the
  # `exit 99` path from a setup_func that lost its `cd`. The
  # `${tmpdir}` is expanded now (not at signal time) on purpose:
  # the variable is function-local and gone by the time the trap fires.
  # shellcheck disable=SC2064
  trap "rm -rf '${tmpdir}'" EXIT INT TERM

  # Per-scenario state, reset before setup.
  TEST_ARGS=()
  TEST_SCRIPT=""
  EXTRA_ASSERT=""

  local actual_exit
  actual_exit=$(
    cd "${tmpdir}" || exit 1
    "${setup_func}" "${tmpdir}"
    local script="${TEST_SCRIPT:-${REAL_SCRIPT}}"
    "${script}" "${TEST_ARGS[@]}" \
      > "${tmpdir}/stdout.txt" 2> "${tmpdir}/stderr.txt" < /dev/null
    echo "$?"
  )

  local actual_err=""
  if [ -f "${tmpdir}/stderr.txt" ]; then
    actual_err=$(cat "${tmpdir}/stderr.txt")
  fi

  local exit_ok=1
  local substr_ok=1
  local extra_ok=1
  [ "${actual_exit}" = "${expected_exit}" ] || exit_ok=0
  if [ -n "${expected_stderr_substr}" ]; then
    printf '%s' "${actual_err}" | grep -qF -- "${expected_stderr_substr}" || substr_ok=0
  fi
  if [ -n "${EXTRA_ASSERT}" ]; then
    ( cd "${tmpdir}" && eval "${EXTRA_ASSERT}" ) || extra_ok=0
  fi

  if [ "${exit_ok}" = 1 ] && [ "${substr_ok}" = 1 ] && [ "${extra_ok}" = 1 ]; then
    echo "PASS: ${name} (exit ${actual_exit})"
    PASS=$((PASS + 1))
  else
    local detail=""
    if [ "${exit_ok}" = 0 ]; then
      detail="expected exit ${expected_exit}, got ${actual_exit}"
    fi
    if [ "${substr_ok}" = 0 ]; then
      [ -n "${detail}" ] && detail="${detail}; "
      detail="${detail}stderr missing substring '${expected_stderr_substr}'"
    fi
    if [ "${extra_ok}" = 0 ]; then
      [ -n "${detail}" ] && detail="${detail}; "
      detail="${detail}extra assertion failed"
    fi
    echo "FAIL: ${name} (${detail})"
    echo "  stderr was:"
    sed 's/^/    /' "${tmpdir}/stderr.txt" 2>/dev/null || true
    FAIL=$((FAIL + 1))
  fi
  # `trap rm -rf` (set above) handles the tmpdir cleanup on both
  # normal exit and signal interruption.
}

# --- Scenarios -------------------------------------------------------------

scenario_01_help() {
  TEST_ARGS=(--help)
}

scenario_02_invalid_version() {
  TEST_ARGS=(notaversion --yes)
}

scenario_03_wrong_branch() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  git checkout -q -b feature-foo
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --yes)
}

scenario_04_dirty_tree() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  echo "dirt" >> Cargo.toml
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --yes)
}

scenario_05_no_signing() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  git config --unset user.signingkey
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --yes)
}

scenario_06_tag_exists_local() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  git tag v0.7.6
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --yes)
}

scenario_07_lab_gate_fail() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  # Replace ledger with comments-only so the gate refuses.
  printf '# Empty ledger\n' > release-gate/lab-validations.txt
  git add -A && git commit -q -m "Empty ledger" && git push -q origin main
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --yes)
  # shellcheck disable=SC2016
  EXTRA_ASSERT='[ -z "$(cd work && git tag --list)" ]'
}

scenario_08_check_tag_version_fail() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  # Bump Cargo.toml past the tag so check-tag-version disagrees.
  sed -i.bak 's/version = "0.7.6"/version = "0.7.7"/' Cargo.toml
  rm -f Cargo.toml.bak
  git add -A && git commit -q -m "Mismatch" && git push -q origin main
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --yes)
  # shellcheck disable=SC2016
  EXTRA_ASSERT='[ -z "$(cd work && git tag --list)" ]'
}

scenario_09_dry_run() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --dry-run)
  # shellcheck disable=SC2016
  EXTRA_ASSERT='[ -z "$(cd work && git tag --list)" ] && [ -z "$(cd remote.git && git tag --list)" ]'
}

scenario_10_happy_path() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --yes)
  # shellcheck disable=SC2016
  EXTRA_ASSERT='(cd work && git rev-parse --verify --quiet refs/tags/v0.7.6 >/dev/null) && (cd remote.git && git tag --list | grep -qx v0.7.6)'
}

scenario_11_push_failure_rollback() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  # pre-receive hook on the bare remote that accepts branch pushes but
  # rejects tag pushes. Lets main land then forces the tag push to fail.
  cat > "${tmpdir}/remote.git/hooks/pre-receive" <<'HOOK'
#!/bin/sh
while read oldref newref refname; do
  case "${refname}" in
    refs/tags/*) echo "tag pushes blocked" >&2; exit 1 ;;
  esac
done
exit 0
HOOK
  chmod +x "${tmpdir}/remote.git/hooks/pre-receive"
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  TEST_ARGS=(v0.7.6 --yes)
  # shellcheck disable=SC2016
  EXTRA_ASSERT='[ -z "$(cd work && git tag --list)" ]'
}

scenario_12_no_tty_no_yes() {
  local tmpdir="$1"
  setup_sandbox "${tmpdir}" "0.7.6"
  cd "${tmpdir}/work" || exit 99
  TEST_SCRIPT="${tmpdir}/work/scripts/release.sh"
  # No --yes, run_scenario pipes stdin from /dev/null so the script
  # sees a non-TTY and refuses without --yes.
  TEST_ARGS=(v0.7.6)
  # shellcheck disable=SC2016
  EXTRA_ASSERT='[ -z "$(cd work && git tag --list)" ]'
}

# --- Drive -----------------------------------------------------------------

run_scenario "01 --help exits 0"                       0 scenario_01_help
run_scenario "02 invalid version rejected"             2 scenario_02_invalid_version
run_scenario "03 wrong branch rejected"                1 scenario_03_wrong_branch "must run from 'main'"
run_scenario "04 dirty tree rejected"                  1 scenario_04_dirty_tree   "working tree is dirty"
run_scenario "05 missing signing identity rejected"    1 scenario_05_no_signing   "requires a signing identity"
run_scenario "06 tag already exists locally rejected"  1 scenario_06_tag_exists_local "already exists locally"
run_scenario "07 lab gate fail blocks tag"             1 scenario_07_lab_gate_fail
run_scenario "08 check-tag-version fail blocks tag"    1 scenario_08_check_tag_version_fail
run_scenario "09 --dry-run mutates nothing"            0 scenario_09_dry_run
run_scenario "10 happy path tags and pushes"           0 scenario_10_happy_path
run_scenario "11 tag-push failure rolls back local"    1 scenario_11_push_failure_rollback "Rolling back the local tag"
run_scenario "12 no-TTY without --yes refused"         1 scenario_12_no_tty_no_yes "not a TTY"

echo ""
echo "Summary: ${PASS} passed, ${FAIL} failed"
[ "${FAIL}" -eq 0 ]
