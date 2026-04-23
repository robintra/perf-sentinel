#!/usr/bin/env bash
#
# Guard against drift between Chart.yaml:appVersion and the image tag
# inside the `artifacthub.io/images` annotation. Both must reference
# the same daemon image version. A PR that bumps appVersion without
# updating the annotation fails helm-ci before merge.
#
# Usage:
#   scripts/check-chart-appversion-annotation.sh

set -euo pipefail

CHART_FILE="charts/perf-sentinel/Chart.yaml"

emit_error() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::error::$*"
  else
    echo "error: $*" >&2
  fi
}

if [ ! -f "${CHART_FILE}" ]; then
  emit_error "Chart file not found at ${CHART_FILE}"
  exit 1
fi

APP_VERSION=$(awk -F'"' '/^appVersion:/ { print $2; exit }' "${CHART_FILE}")
IMAGE_TAG=$(awk '
  /^[[:space:]]+image: ghcr\.io\/robintra\/perf-sentinel:/ {
    sub(/.*:/, "")
    gsub(/[[:space:]]/, "")
    print
    exit
  }
' "${CHART_FILE}")

if [ -z "${APP_VERSION}" ]; then
  emit_error "Could not parse appVersion from ${CHART_FILE}"
  exit 1
fi
if [ -z "${IMAGE_TAG}" ]; then
  emit_error "Could not parse artifacthub.io/images image tag from ${CHART_FILE}"
  exit 1
fi

if [ "${APP_VERSION}" != "${IMAGE_TAG}" ]; then
  emit_error "Chart.yaml appVersion (${APP_VERSION}) does not match the artifacthub.io/images image tag (${IMAGE_TAG}). Bump the annotation when bumping appVersion so Artifact Hub advertises the correct daemon image."
  exit 1
fi

echo "appVersion and artifacthub.io/images tag both at ${APP_VERSION}"
