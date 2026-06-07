#!/usr/bin/env bash
# Constrained tag-and-push path for perf-sentinel: implements steps 5
# and 6 of docs/RELEASE-PROCEDURE.md as a single fail-closed command.
# Refuses to tag unless every pre-check and gate passes. The tag is
# always signed (`git tag -s`), no bypass. The lab-validation gate is
# the sole exception: --skip-lab skips it explicitly and logs a loud
# audit warning, for releases validated by other means (e.g. a
# CLI/docs-only change covered by the E2E suite).

set -euo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

VERSION=""
DRY_RUN=0
YES=0
SKIP_LAB=0

emit_error() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*"
  else
    echo "release: $*" >&2
  fi
}

emit_notice() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::notice::$*"
  else
    echo "$*"
  fi
}

usage() {
  cat <<EOF
Usage: $(basename "$0") vX.Y.Z [--dry-run] [--yes] [--skip-lab]

Implements steps 5 (lab-validation gate) and 6 (signed tag + push) of
docs/RELEASE-PROCEDURE.md. Fails closed: every pre-check and gate must
pass before any tag is created or pushed. The tag is always signed,
no bypass. The lab gate is the sole skippable gate, via --skip-lab.

Options:
  --dry-run   Run every gate, print the planned action, mutate nothing.
  --yes       Skip the interactive confirmation before tag and push.
  --skip-lab  Bypass the lab-validation gate explicitly. Logs a loud
              audit warning and never writes the ledger. For releases
              validated by other means, e.g. a CLI/docs-only change
              covered by the E2E suite. All other pre-checks and the
              version gate still apply.
  -h, --help  Print this help and exit.

Exit codes:
  0  Release tagged and pushed (or --dry-run would succeed).
  1  Pre-check or gate failed, nothing was mutated.
  2  Usage error.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --dry-run)  DRY_RUN=1; shift ;;
    --yes)      YES=1; shift ;;
    --skip-lab) SKIP_LAB=1; shift ;;
    --)        shift; break ;;
    -*)        emit_error "unknown option: $1"; usage >&2; exit 2 ;;
    *)
      if [ -n "${VERSION}" ]; then
        emit_error "unexpected positional argument '$1' (version already set to '${VERSION}')"
        usage >&2
        exit 2
      fi
      VERSION="$1"
      shift
      ;;
  esac
done

if [ -z "${VERSION}" ]; then
  emit_error "version argument is required"
  usage >&2
  exit 2
fi

# Same regex shape as release-gate/check-lab-validation.sh.
if ! [[ "${VERSION}" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  emit_error "version must match vX.Y.Z or X.Y.Z (optionally with -suffix), got '${VERSION}'"
  exit 2
fi
[[ "${VERSION}" == v* ]] || VERSION="v${VERSION}"

# Pre-check: repo root markers.
if [ ! -f "${REPO_ROOT}/Cargo.toml" ] \
   || [ ! -d "${REPO_ROOT}/release-gate" ] \
   || [ ! -x "${REPO_ROOT}/scripts/check-tag-version.sh" ]; then
  emit_error "must run from a perf-sentinel checkout (missing Cargo.toml, release-gate/, or scripts/check-tag-version.sh)"
  exit 1
fi
cd "${REPO_ROOT}"

# Pre-check: branch == main.
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "${CURRENT_BRANCH}" != "main" ]; then
  emit_error "must run from 'main' (current branch: '${CURRENT_BRANCH}'). Merge the release branch into main first."
  exit 1
fi

# Pre-check: clean working tree (porcelain=v1 pins the contract).
if [ -n "$(git status --porcelain=v1)" ]; then
  emit_error "working tree is dirty. Commit or stash changes before tagging."
  exit 1
fi

# Pre-check: signing identity. `git tag -s` is GPG by default, falls
# back to SSH when `gpg.format = ssh`. Both modes require
# `user.signingkey` to be set.
SIGNINGKEY="$(git config --get user.signingkey 2>/dev/null || true)"
if [ -z "${SIGNINGKEY}" ]; then
  emit_error "git tag -s requires a signing identity. Configure user.signingkey (GPG) or 'gpg.format ssh' + user.signingkey (SSH). See docs/RELEASE-PROCEDURE.md."
  exit 1
fi

# Pre-check: remote sync. `main` may be ahead (the release commits)
# but must not be behind `origin/main`.
if ! git fetch origin --quiet; then
  emit_error "git fetch origin failed. Check network or remote configuration before tagging."
  exit 1
fi
LOCAL_SHA="$(git rev-parse HEAD)"
REMOTE_SHA="$(git rev-parse origin/main 2>/dev/null || echo '')"
if [ -z "${REMOTE_SHA}" ]; then
  emit_error "could not resolve origin/main. Is the remote configured?"
  exit 1
fi
BEHIND_COUNT="$(git rev-list --count "${LOCAL_SHA}..${REMOTE_SHA}")"
if [ "${BEHIND_COUNT}" -gt 0 ]; then
  emit_error "local main is ${BEHIND_COUNT} commit(s) behind origin/main. Pull first."
  exit 1
fi

# Pre-check: tag does not already exist locally or on the remote.
# Capture ls-remote output before grep so a transient network failure
# never silently looks like "tag absent on remote".
if git rev-parse --verify --quiet "refs/tags/${VERSION}" >/dev/null; then
  emit_error "tag ${VERSION} already exists locally. Retagging is a deliberate act, outside the scope of this script."
  exit 1
fi
REMOTE_TAG_LISTING=""
if ! REMOTE_TAG_LISTING="$(git ls-remote --tags origin "refs/tags/${VERSION}" 2>/dev/null)"; then
  emit_error "git ls-remote --tags origin failed. Check network or remote configuration before tagging."
  exit 1
fi
if printf '%s\n' "${REMOTE_TAG_LISTING}" | grep -q "refs/tags/${VERSION}$"; then
  emit_error "tag ${VERSION} already exists on origin. Retagging is a deliberate act, outside the scope of this script."
  exit 1
fi

# Gate: workspace Cargo.toml versions match the tag. Subprocess stderr
# is already actionable, relay it as-is.
if ! "${REPO_ROOT}/scripts/check-tag-version.sh" "${VERSION}"; then
  emit_error "scripts/check-tag-version.sh failed. Bump the workspace version to match ${VERSION} first."
  exit 1
fi

# Gate: lab validation ledger has a fresh PASS for this version. The
# operator can skip this single gate with --skip-lab, which logs a loud
# audit warning instead of consulting the ledger. The ledger is never
# written here, so a skipped lab leaves no false PASS behind.
if [ "${SKIP_LAB}" -eq 1 ]; then
  emit_notice "release: WARNING lab-validation gate bypassed by operator (--skip-lab). ${VERSION} was NOT validated in the simulation lab, and no PASS was recorded in the ledger."
elif ! "${REPO_ROOT}/release-gate/check-lab-validation.sh" --version "${VERSION}"; then
  emit_error "release-gate/check-lab-validation.sh refused ${VERSION}. Run the lab and append a PASS entry to release-gate/lab-validations.txt (step 4 of RELEASE-PROCEDURE.md), or pass --skip-lab to bypass this gate explicitly."
  exit 1
fi

SHORT_SHA="$(git rev-parse --short HEAD)"
TAG_MESSAGE="${VERSION}"

if [ "${DRY_RUN}" -eq 1 ]; then
  emit_notice "release: --dry-run, no mutation. Plan:"
  emit_notice "  create signed tag ${VERSION} at ${SHORT_SHA} with message '${TAG_MESSAGE}'"
  emit_notice "  push main to origin"
  emit_notice "  push tag ${VERSION} to origin"
  exit 0
fi

# Confirmation. Refuse non-interactively unless --yes is explicit (CI
# safety: a piped invocation should never tag silently).
if [ "${YES}" -ne 1 ]; then
  if [ ! -t 0 ]; then
    emit_error "interactive confirmation requested but stdin is not a TTY. Pass --yes to confirm non-interactively."
    exit 1
  fi
  if [ "${SKIP_LAB}" -eq 1 ]; then
    printf 'WARNING: lab-validation gate skipped (--skip-lab), %s was NOT lab-validated.\n' "${VERSION}"
  fi
  printf 'Tag %s at %s and push to origin?\n  Message: %s\nProceed? [y/N] ' "${VERSION}" "${SHORT_SHA}" "${TAG_MESSAGE}"
  read -r reply </dev/tty
  case "${reply}" in
    y|Y|yes|YES) ;;
    *) emit_error "aborted by operator, nothing mutated"; exit 1 ;;
  esac
fi

git tag -s "${VERSION}" -m "${TAG_MESSAGE}"
emit_notice "tag ${VERSION} created locally"

# Push main first so the tag never references a commit absent from origin.
if ! git push origin main; then
  emit_error "git push origin main failed. Roll back the local tag with: git tag -d ${VERSION}"
  exit 1
fi
emit_notice "main pushed to origin"

# Push the tag. On failure, roll back the local tag to avoid a dangling
# ref. Never delete a tag that may have made it to the remote.
if ! git push origin "${VERSION}"; then
  emit_error "git push origin ${VERSION} failed. Rolling back the local tag."
  if git tag -d "${VERSION}"; then
    emit_error "local tag ${VERSION} deleted. Retry with: $(basename "$0") ${VERSION} --yes"
  else
    emit_error "rollback failed: local tag ${VERSION} may still exist. Inspect with 'git tag --list ${VERSION}' and delete manually before retrying."
  fi
  exit 1
fi

emit_notice ""
emit_notice "Released ${VERSION} at ${SHORT_SHA}"
emit_notice "  .github/workflows/release.yml is now running."
emit_notice "  Next manual step (RELEASE-PROCEDURE.md):"
emit_notice "    7. Release the Helm chart: scripts/release-chart.sh chart-vA.B.C (once the GHCR image lands)."
exit 0
