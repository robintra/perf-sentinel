#!/usr/bin/env bash
#
# Install perf-sentinel git hooks. Idempotent: re-running overwrites
# the symlink to the latest hook script in scripts/hooks/.
#
# Hooks are tracked in scripts/hooks/ so they version with the repo.
# `.git/hooks/` is per-clone and not version-controlled, hence the
# symlink indirection.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOK_SOURCE_DIR="${REPO_ROOT}/scripts/hooks"
HOOK_DEST_DIR="${REPO_ROOT}/.git/hooks"

if [ ! -d "${HOOK_SOURCE_DIR}" ]; then
  echo "error: ${HOOK_SOURCE_DIR} not found; run from the repo root?" >&2
  exit 1
fi

# Detect a non-default core.hooksPath. If set globally or locally to
# anything other than `.git/hooks`, git will not run the hooks we
# install here. We don't change the config behind your back; we report
# the situation and ask you to choose.
EXISTING_HOOKS_PATH="$(git config --get core.hooksPath 2>/dev/null || true)"
if [ -n "${EXISTING_HOOKS_PATH}" ]; then
  echo "warning: git is configured with core.hooksPath=${EXISTING_HOOKS_PATH}" >&2
  echo "         this overrides .git/hooks/, so the symlinks below will NOT execute." >&2
  echo >&2
  echo "Pick one of:" >&2
  echo "  1) override for this repo only (recommended):" >&2
  echo "       git config --local --unset core.hooksPath" >&2
  echo "       bash scripts/install-hooks.sh" >&2
  echo "  2) chain the gitleaks check into your existing global hook:" >&2
  echo "       see scripts/hooks/pre-commit for the gitleaks invocation" >&2
  echo >&2
  echo "install-hooks.sh has not modified your git config. Aborting." >&2
  exit 1
fi

mkdir -p "${HOOK_DEST_DIR}"

for hook_path in "${HOOK_SOURCE_DIR}"/*; do
  [ -e "${hook_path}" ] || continue
  hook_name="$(basename "${hook_path}")"
  dest="${HOOK_DEST_DIR}/${hook_name}"
  ln -sf "../../scripts/hooks/${hook_name}" "${dest}"
  echo "installed: ${dest} -> scripts/hooks/${hook_name}"
done

echo
echo "Hooks installed. Run 'git commit --no-verify' to bypass them when needed."
