#!/usr/bin/env bash
# Constrained tag-and-push path for the Helm chart: implements step 7
# of docs/RELEASE-PROCEDURE.md as a single fail-closed command. Refuses
# to tag unless every pre-check and gate passes. The tag is always
# signed (`git tag -s`), no bypass. Pushing the tag triggers
# .github/workflows/helm-release.yml, which packages the chart, pushes
# it to GHCR as an OCI artifact, cosign-signs it, attests SLSA build
# provenance and an SPDX SBOM, then drafts the GitHub Release. This
# script does not certify anything itself, it only creates the signed
# trigger.

set -euo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

VERSION=""
DRY_RUN=0
YES=0
SKIP_IMAGE_CHECK=0

CHART_FILE="${REPO_ROOT}/charts/perf-sentinel/Chart.yaml"
IMAGE_REPO="ghcr.io/robintra/perf-sentinel"

emit_error() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*"
  else
    echo "release-chart: $*" >&2
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
Usage: $(basename "$0") chart-vX.Y.Z [--dry-run] [--yes] [--skip-image-check]

Implements step 7 of docs/RELEASE-PROCEDURE.md: the signed chart tag
that triggers .github/workflows/helm-release.yml. Fails closed: every
pre-check and gate must pass before any tag is created or pushed. The
tag is always signed, no bypass. The leading 'chart-v' is optional,
'X.Y.Z' is accepted and normalized.

Options:
  --dry-run            Run every gate, print the planned action, mutate nothing.
  --yes                Skip the interactive confirmation before tag and push.
  --skip-image-check   Do not verify the daemon image exists on GHCR.
  -h, --help           Print this help and exit.

Exit codes:
  0  Chart tagged and pushed (or --dry-run would succeed).
  1  Pre-check or gate failed, nothing was mutated.
  2  Usage error.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help)          usage; exit 0 ;;
    --dry-run)          DRY_RUN=1; shift ;;
    --yes)              YES=1; shift ;;
    --skip-image-check) SKIP_IMAGE_CHECK=1; shift ;;
    --)                 shift; break ;;
    -*)                 emit_error "unknown option: $1"; usage >&2; exit 2 ;;
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

# Normalize: strip an optional leading 'chart-v', validate the bare
# semver-ish version (same shape as scripts/release.sh, minus the 'v'
# prefix), then reconstruct the canonical 'chart-v' tag.
BARE_VERSION="${VERSION#chart-v}"
if ! [[ "${BARE_VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  emit_error "version must match chart-vX.Y.Z or X.Y.Z (optionally with -suffix), got '${VERSION}'"
  exit 2
fi
TAG="chart-v${BARE_VERSION}"

# Pre-check: repo root markers.
if [ ! -f "${REPO_ROOT}/Cargo.toml" ] \
   || [ ! -f "${CHART_FILE}" ] \
   || [ ! -x "${REPO_ROOT}/scripts/check-helm-tag-version.sh" ]; then
  emit_error "must run from a perf-sentinel checkout (missing Cargo.toml, ${CHART_FILE#"${REPO_ROOT}/"}, or scripts/check-helm-tag-version.sh)"
  exit 1
fi
cd "${REPO_ROOT}"

# Pre-check: branch == main.
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "${CURRENT_BRANCH}" != "main" ]; then
  emit_error "must run from 'main' (current branch: '${CURRENT_BRANCH}'). The chart tag follows the binary release on main."
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

# Pre-check: remote sync. Unlike scripts/release.sh, this script does
# not push 'main' (it is already on origin from step 6). The tagged
# commit must therefore already exist on origin/main, so require an
# exact match: refuse if local main is ahead or behind.
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
if [ "${LOCAL_SHA}" != "${REMOTE_SHA}" ]; then
  AHEAD_COUNT="$(git rev-list --count "${REMOTE_SHA}..${LOCAL_SHA}")"
  BEHIND_COUNT="$(git rev-list --count "${LOCAL_SHA}..${REMOTE_SHA}")"
  emit_error "local main is not in sync with origin/main (ahead ${AHEAD_COUNT}, behind ${BEHIND_COUNT}). The chart tag must point at a commit already on origin/main. Push main first (or run scripts/release.sh), then retry."
  exit 1
fi

# Pre-check: tag does not already exist locally or on the remote.
# Capture ls-remote output before grep so a transient network failure
# never silently looks like "tag absent on remote".
if git rev-parse --verify --quiet "refs/tags/${TAG}" >/dev/null; then
  emit_error "tag ${TAG} already exists locally. Retagging is a deliberate act, outside the scope of this script."
  exit 1
fi
REMOTE_TAG_LISTING=""
if ! REMOTE_TAG_LISTING="$(git ls-remote --tags origin "refs/tags/${TAG}" 2>/dev/null)"; then
  emit_error "git ls-remote --tags origin failed. Check network or remote configuration before tagging."
  exit 1
fi
if printf '%s\n' "${REMOTE_TAG_LISTING}" | grep -q "refs/tags/${TAG}$"; then
  emit_error "tag ${TAG} already exists on origin. Retagging is a deliberate act, outside the scope of this script."
  exit 1
fi

# Gate: chart tag matches charts/perf-sentinel/Chart.yaml:version.
# Subprocess stderr is already actionable, relay it as-is.
if ! "${REPO_ROOT}/scripts/check-helm-tag-version.sh" "${TAG}"; then
  emit_error "scripts/check-helm-tag-version.sh failed. Bump the chart version to match ${TAG} first."
  exit 1
fi

# Resolve whether the daemon image tag the chart pins (its appVersion)
# exists on GHCR. Echoes 'present', 'absent', or 'unknown'. The
# anonymous Registry v2 API is authoritative: HTTP 200 = present,
# 404 = absent, anything else = unknown (network, rate limit, auth).
# It only needs curl, which is ubiquitous, and works without a login
# because the image is public. crane and docker are positive-only
# fallbacks if curl is missing: a non-zero exit there can mean absent
# or network, so they confirm presence but never assert absence, to
# avoid a false refusal on a transient blip.
check_ghcr_image() {
  local repo="${IMAGE_REPO#ghcr.io/}"
  local ref="${IMAGE_REPO}:${APP_VERSION}"
  local accept='application/vnd.oci.image.index.v1+json,application/vnd.docker.distribution.manifest.list.v2+json,application/vnd.docker.distribution.manifest.v2+json,application/vnd.oci.image.manifest.v1+json'
  if command -v curl >/dev/null 2>&1; then
    local tok_resp token code
    tok_resp="$(curl -fsSL "https://ghcr.io/token?service=ghcr.io&scope=repository:${repo}:pull" 2>/dev/null || true)"
    token="$(printf '%s' "${tok_resp}" | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')"
    if [ -n "${token}" ]; then
      code="$(curl -sS -o /dev/null -w '%{http_code}' \
        -H "Authorization: Bearer ${token}" \
        -H "Accept: ${accept}" \
        "https://ghcr.io/v2/${repo}/manifests/${APP_VERSION}" 2>/dev/null || echo 000)"
      case "${code}" in
        200) echo present; return ;;
        404) echo absent;  return ;;
      esac
    fi
  fi
  if command -v crane >/dev/null 2>&1 && crane manifest "${ref}" >/dev/null 2>&1; then
    echo present
    return
  fi
  if command -v docker >/dev/null 2>&1 && docker manifest inspect "${ref}" >/dev/null 2>&1; then
    echo present
    return
  fi
  echo unknown
}

# Gate: the daemon image the chart pins must already exist on GHCR, so
# `helm install` does not pull a missing image. --skip-image-check
# bypasses it (escape hatch for an offline tag or a non-standard flow).
APP_VERSION="$(awk -F'"' '/^appVersion:/ { print $2; exit }' "${CHART_FILE}")"
if [ -z "${APP_VERSION}" ]; then
  emit_error "could not parse appVersion from ${CHART_FILE}"
  exit 1
fi
IMAGE_REF="${IMAGE_REPO}:${APP_VERSION}"
if [ "${SKIP_IMAGE_CHECK}" -eq 1 ]; then
  emit_notice "release-chart: --skip-image-check, not verifying ${IMAGE_REF} exists on GHCR"
else
  case "$(check_ghcr_image)" in
    present)
      emit_notice "GHCR image ${IMAGE_REF} found"
      ;;
    absent)
      emit_error "GHCR image ${IMAGE_REF} not found. Wait for release.yml to publish the daemon image (step 6 of RELEASE-PROCEDURE.md), or pass --skip-image-check to tag anyway."
      exit 1
      ;;
    *)
      emit_error "could not confirm ${IMAGE_REF} exists (no usable GHCR query tool: curl, crane or docker, or the registry was unreachable). Install one and retry, or pass --skip-image-check."
      exit 1
      ;;
  esac
fi

SHORT_SHA="$(git rev-parse --short HEAD)"
TAG_MESSAGE="${TAG}"

if [ "${DRY_RUN}" -eq 1 ]; then
  emit_notice "release-chart: --dry-run, no mutation. Plan:"
  emit_notice "  create signed tag ${TAG} at ${SHORT_SHA} with message '${TAG_MESSAGE}'"
  emit_notice "  push tag ${TAG} to origin"
  exit 0
fi

# Confirmation. Refuse non-interactively unless --yes is explicit (CI
# safety: a piped invocation should never tag silently).
if [ "${YES}" -ne 1 ]; then
  if [ ! -t 0 ]; then
    emit_error "interactive confirmation requested but stdin is not a TTY. Pass --yes to confirm non-interactively."
    exit 1
  fi
  printf 'Tag %s at %s and push to origin?\n  Message: %s\nProceed? [y/N] ' "${TAG}" "${SHORT_SHA}" "${TAG_MESSAGE}"
  read -r reply </dev/tty
  case "${reply}" in
    y|Y|yes|YES) ;;
    *) emit_error "aborted by operator, nothing mutated"; exit 1 ;;
  esac
fi

git tag -s "${TAG}" -m "${TAG_MESSAGE}"
emit_notice "tag ${TAG} created locally"

# Push the tag only. 'main' is already on origin (verified in sync
# above), so there is nothing else to push. On failure, roll back the
# local tag to avoid a dangling ref. Never delete a tag that may have
# made it to the remote.
if ! git push origin "${TAG}"; then
  emit_error "git push origin ${TAG} failed. Rolling back the local tag."
  if git tag -d "${TAG}"; then
    emit_error "local tag ${TAG} deleted. Retry with: $(basename "$0") ${TAG} --yes"
  else
    emit_error "rollback failed: local tag ${TAG} may still exist. Inspect with 'git tag --list ${TAG}' and delete manually before retrying."
  fi
  exit 1
fi

emit_notice ""
emit_notice "Released ${TAG} at ${SHORT_SHA}"
emit_notice "  .github/workflows/helm-release.yml is now running. It will:"
emit_notice "    - package the chart and push it to ghcr.io/robintra/charts as an OCI artifact"
emit_notice "    - cosign keyless sign the artifact"
emit_notice "    - attest SLSA build provenance (gh attestation verify)"
emit_notice "    - generate and attest an SPDX SBOM"
emit_notice "    - draft the GitHub Release for ${TAG}"
emit_notice "  Publish the drafted Release once you have reviewed it."
exit 0
