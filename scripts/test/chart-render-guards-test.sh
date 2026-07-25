#!/usr/bin/env bash
#
# Self-test for the chart's render-time guards.
#
# Usage:
#   scripts/test/chart-render-guards-test.sh
#
# Called by .github/workflows/helm-ci.yml, also runnable locally.
#
# Every guard here exists because the rendered config would otherwise be
# one the daemon refuses at startup, or a silent no-op the operator only
# discovers through a 503. All of them live in templates/configmap.yaml.
#
# Each scenario must fail against a chart without the guard it covers,
# otherwise it pins nothing. Renders are captured with `if ! out=$(...)`
# so an unexpected failure reports FAIL and the run continues, instead of
# `set -e` killing the harness before the summary.
#
# Exit codes:
#   0 - every scenario matches
#   1 - a scenario failed, or helm is missing

set -euo pipefail

CHART_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/charts/perf-sentinel"
PERSIST=(--set workload.kind=StatefulSet --set workload.statefulset.persistence.enabled=true)
failures=0

command -v helm >/dev/null || { echo "error: helm not found" >&2; exit 1; }

report() {
  if [ "$1" = "pass" ]; then
    echo "PASS: $2"
  else
    echo "FAIL: $2"
    failures=$((failures + 1))
  fi
}

# Render only the ConfigMap, on stdout. Extra args are passed to helm.
render_config() {
  helm template t "$CHART_DIR" --show-only templates/configmap.yaml "$@" 2>&1
}

# Assert the render succeeds and its output satisfies a predicate given as
# a shell snippet reading $out.
expect_render() {
  local desc="$1" predicate="$2"; shift 2
  local out
  if ! out=$(render_config "$@"); then
    report fail "$desc (render failed: $(head -1 <<<"$out"))"
    return
  fi
  if eval "$predicate"; then
    report pass "$desc"
  else
    report fail "$desc"
  fi
}

# Assert the render fails and the message mentions $2.
expect_fail() {
  local desc="$1" needle="$2"; shift 2
  local out
  if out=$(render_config "$@"); then
    report fail "$desc (render unexpectedly succeeded)"
  elif grep -q "$needle" <<<"$out"; then
    report pass "$desc"
  else
    report fail "$desc (wrong message: $(head -1 <<<"$out"))"
  fi
}

# --- TOML table injection ---------------------------------------------------

# 1. Persistence on, operator declares neither table: the chart injects both.
expect_render "injects both tables when config.toml declares neither" \
  '[ "$(grep -c "^\s*\[daemon\.ack\]" <<<"$out")" = 1 ] &&
   [ "$(grep -c "^\s*\[daemon\.archive\]" <<<"$out")" = 1 ] &&
   grep -q "storage_path = \"/var/lib/perf-sentinel/acks.jsonl\"" <<<"$out"' \
  "${PERSIST[@]}"

# 2. A table opened by a header stops the injection with a pointer to the
#    opt-out. Guard removed, the chart would append a second table.
expect_fail "refuses to append when a table header is present" "manageDaemonPaths" \
  "${PERSIST[@]}" --set-string 'config.toml=[daemon.ack]
api_key = "0123456789abcdef"'

# 3. Same for the dotted-key spelling, which opens the table just as much and
#    which a header-only regex misses.
expect_fail "refuses to append on a dotted-key table" "manageDaemonPaths" \
  "${PERSIST[@]}" --set-string 'config.toml=[daemon]
ack.storage_path = "/var/lib/perf-sentinel/acks.jsonl"'

# 4. And for the inline-table spelling.
expect_fail "refuses to append on an inline table" "manageDaemonPaths" \
  "${PERSIST[@]}" --set-string 'config.toml=[daemon]
archive = { path = "/var/lib/perf-sentinel/archive.ndjson" }'

# 5. manageDaemonPaths=false hands the whole job over: the operator's tables
#    survive untouched and the chart appends nothing.
expect_render "leaves both tables alone when manageDaemonPaths is false" \
  '[ "$(grep -c "^\s*\[daemon\.ack\]" <<<"$out")" = 1 ] &&
   grep -q "toml_path" <<<"$out" &&
   ! grep -q "^\s*\[daemon\.archive\]" <<<"$out"' \
  "${PERSIST[@]}" --set workload.statefulset.persistence.manageDaemonPaths=false \
  --set-string 'config.toml=[daemon.ack]
toml_path = "/etc/perf-sentinel/acks/.perf-sentinel-acknowledgments.toml"
storage_path = "/var/lib/perf-sentinel/acks.jsonl"'

# 6. Deployment: no injection at all, an operator table stays untouched.
expect_render "injects nothing without persistence" \
  '[ "$(grep -c "^\s*\[daemon\.ack\]" <<<"$out")" = 1 ] &&
   ! grep -q "^\s*\[daemon\.archive\]" <<<"$out"' \
  --set-string 'config.toml=[daemon.ack]
api_key = "0123456789abcdef"'

# --- Archive against green scoring ------------------------------------------

# 7. [green] enabled = false: the archive injection is skipped, the daemon
#    rejects an archive with no energy data. The ack table still lands.
expect_render "skips the archive when green scoring is off" \
  '! grep -q "^\s*\[daemon\.archive\]" <<<"$out" && grep -q "^\s*\[daemon\.ack\]" <<<"$out"' \
  "${PERSIST[@]}" --set-string 'config.toml=[green]
enabled = false'

# 8. Same, with a bracket in a comment between the header and the key. A
#    "[^[]*" style regex stops at that bracket and misses the setting.
expect_render "sees green off past a bracket in a comment" \
  '! grep -q "^\s*\[daemon\.archive\]" <<<"$out"' \
  "${PERSIST[@]}" --set-string 'config.toml=[green]
# region list lives under [green] in docs/CONFIGURATION.md
enabled = false'

# 9. Declaring the archive with green off never starts, whatever the workload
#    kind, so the guard must fire on a plain Deployment too.
expect_fail "fails on a declared archive with green off, without persistence" "green" \
  --set-string 'config.toml=[green]
enabled = false

[daemon.archive]
path = "/var/lib/perf-sentinel/archive.ndjson"'

# --- Port consistency -------------------------------------------------------

# 10. Service port moved without moving the daemon bind.
expect_fail "fails when the http Service port leaves the daemon bind behind" \
  "listen_port_http" --set service.ports.otlpHttp.port=8080 \
  --set-string 'config.toml=[daemon]
listen_port_http = 4318'

# 11. Same for gRPC, which a copy-paste of the http block would leave
#     comparing the wrong side of the pair.
expect_fail "fails when the grpc Service port leaves the daemon bind behind" \
  "listen_port_grpc" --set service.ports.otlpGrpc.port=9317 \
  --set-string 'config.toml=[daemon]
listen_port_grpc = 4317'

# 12. The key omitted entirely is not a free pass: the daemon still binds its
#     4318 default, so the mismatch is just as real.
expect_fail "fails when the port is left at the daemon default" "4318" \
  --set service.ports.otlpHttp.port=8080 --set-string 'config.toml=[daemon]
listen_address = "0.0.0.0"'

# 13. Both moved together: accepted.
expect_render "accepts a port change applied on both sides" 'true' \
  --set service.ports.otlpHttp.port=8080 --set-string 'config.toml=[daemon]
listen_port_http = 8080'

# --- Persistence outside StatefulSet ----------------------------------------

# 14. Persistence asked for on a Deployment: nothing would be mounted.
expect_fail "fails on persistence outside StatefulSet" "workload.kind" \
  --set workload.statefulset.persistence.enabled=true

if [ "$failures" -eq 0 ]; then
  echo "All scenarios passed."
  exit 0
fi
echo "$failures scenario(s) failed." >&2
exit 1
