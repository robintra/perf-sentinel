#!/usr/bin/env bash
#
# Verify a release tag matches every Cargo.toml in the workspace.
#
# Usage:
#   scripts/check-tag-version.sh v0.5.3
#   scripts/check-tag-version.sh 0.5.3         # leading `v` optional
#
# Called by .github/workflows/release.yml as the first gate of the
# release flow, also runnable locally before tagging to catch drift
# without pushing.
#
# Behavior:
#   1. Read `workspace.package.version` from the root Cargo.toml.
#   2. Read the effective `[package].version` of each crates/*/Cargo.toml.
#      A crate inheriting via `version.workspace = true` (or the inline
#      table form `version = { workspace = true }`) resolves to the
#      workspace version. A crate with a hardcoded version must match
#      the target directly.
#   3. Fail loudly if any of the above disagrees with the target tag.
#
# Exit codes:
#   0 - every Cargo.toml matches the target version
#   1 - mismatch, missing version field, or no crates found

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
  echo "usage: $(basename "$0") <tag-or-version>" >&2
  exit 1
fi

# Strip an optional leading `v` so both `v0.5.3` and `0.5.3` are accepted.
TARGET_VERSION="${1#v}"

# Section-scoped awk: avoids accidental matches on dependency version
# pins elsewhere in the file.
WORKSPACE_VERSION=$(awk -F\" '
  /^\[workspace\.package\]/ { in_section = 1; next }
  /^\[/                     { in_section = 0 }
  in_section && /^version[[:space:]]*=/ { print $2; exit }
' Cargo.toml)

if [ -z "${WORKSPACE_VERSION}" ]; then
  emit_error "Could not extract workspace.package.version from the root Cargo.toml"
  exit 1
fi
if [ "${TARGET_VERSION}" != "${WORKSPACE_VERSION}" ]; then
  emit_error "Tag v${TARGET_VERSION} does not match workspace.package.version (${WORKSPACE_VERSION}) in Cargo.toml. Bump the workspace version before tagging."
  exit 1
fi

# Enumerate crate Cargo.toml files. nullglob keeps CRATE_TOMLS empty
# if the glob matches nothing, so we can catch that explicitly rather
# than letting awk fail later with a cryptic "cannot open file".
shopt -s nullglob
CRATE_TOMLS=(crates/*/Cargo.toml)
shopt -u nullglob

if [ "${#CRATE_TOMLS[@]}" -eq 0 ]; then
  emit_error "No crates/*/Cargo.toml files found; is the script running from the repository root?"
  exit 1
fi

FAIL=0
for CRATE_TOML in "${CRATE_TOMLS[@]}"; do
  PKG_VERSION=$(awk -F\" '
    /^\[package\]/ { in_section = 1; next }
    /^\[/          { in_section = 0 }
    in_section && /^version[[:space:]]*=[[:space:]]*"/ {
      print $2; found = 1; exit
    }
    in_section && /^version[[:space:]]*\.workspace[[:space:]]*=[[:space:]]*true/ {
      print "WORKSPACE"; found = 1; exit
    }
    in_section && /^version[[:space:]]*=[[:space:]]*\{[[:space:]]*workspace[[:space:]]*=[[:space:]]*true/ {
      print "WORKSPACE"; found = 1; exit
    }
    END { if (!found) exit 1 }
  ' "${CRATE_TOML}") || {
    emit_error "Could not extract [package].version from ${CRATE_TOML}"
    FAIL=1
    continue
  }

  if [ "${PKG_VERSION}" = "WORKSPACE" ]; then
    EFFECTIVE="${WORKSPACE_VERSION}"
  else
    EFFECTIVE="${PKG_VERSION}"
  fi

  if [ "${EFFECTIVE}" != "${TARGET_VERSION}" ]; then
    emit_error "${CRATE_TOML} effective version ${EFFECTIVE} does not match tag v${TARGET_VERSION}"
    FAIL=1
  else
    emit_notice "${CRATE_TOML} -> ${EFFECTIVE} (matches tag)"
  fi
done

# Verify intra-workspace dependency pins also match the target version.
# A crate that publishes to crates.io must pin its sibling workspace
# crate via `version = "X.Y.Z"` next to `path = "..."` so cargo publish
# can resolve the dependency from the registry. If [workspace.package].version
# is bumped without bumping these pins, cargo publish either fails (registry
# not yet propagated) or silently publishes a binary linked to the previous
# core version.
INTRA_WORKSPACE_CRATES=()
for CRATE_TOML in "${CRATE_TOMLS[@]}"; do
  NAME=$(awk -F\" '
    /^\[package\]/ { in_section = 1; next }
    /^\[/          { in_section = 0 }
    in_section && /^name[[:space:]]*=[[:space:]]*"/ { print $2; exit }
  ' "${CRATE_TOML}")
  if [ -n "${NAME}" ]; then
    INTRA_WORKSPACE_CRATES+=("${NAME}")
  fi
done

for CRATE_TOML in "${CRATE_TOMLS[@]}"; do
  for DEP_NAME in "${INTRA_WORKSPACE_CRATES[@]}"; do
    DEP_VERSION=$(awk -v dep="${DEP_NAME}" '
      $0 ~ "^"dep"[[:space:]]*=[[:space:]]*\\{" {
        if (match($0, /version[[:space:]]*=[[:space:]]*"=?[^"]+"/)) {
          v = substr($0, RSTART, RLENGTH)
          sub(/^version[[:space:]]*=[[:space:]]*"/, "", v)
          sub(/"$/, "", v)
          sub(/^=/, "", v)
          print v
          exit
        }
      }
    ' "${CRATE_TOML}")

    if [ -n "${DEP_VERSION}" ] && [ "${DEP_VERSION}" != "${TARGET_VERSION}" ]; then
      emit_error "${CRATE_TOML} dependency ${DEP_NAME} pinned to ${DEP_VERSION}, expected ${TARGET_VERSION}"
      FAIL=1
    elif [ -n "${DEP_VERSION}" ]; then
      emit_notice "${CRATE_TOML} dep ${DEP_NAME} -> ${DEP_VERSION} (matches tag)"
    fi
  done
done

if [ "${FAIL}" -ne 0 ]; then
  exit 1
fi
emit_notice "Tag v${TARGET_VERSION} matches every Cargo.toml in the workspace"
