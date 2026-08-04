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
# one the daemon refuses at startup, a silent no-op the operator only
# discovers through a 503, or a topology that degrades detection without
# saying so. Most live in templates/configmap.yaml, the DaemonSet one in
# templates/daemonset.yaml.
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

# Same for the fragments ConfigMap. Needed for the success cases: the failure
# cases abort the whole render and are observable through render_config.
render_fragments() {
  helm template t "$CHART_DIR" --show-only templates/configmap-fragments.yaml "$@" 2>&1
}

# expect_render against the fragments ConfigMap.
expect_fragments() {
  local desc="$1" predicate="$2"; shift 2
  local out
  if ! out=$(render_fragments "$@"); then
    report fail "$desc (render failed: $(head -1 <<<"$out"))"
    return
  fi
  if eval "$predicate"; then
    report pass "$desc"
  else
    report fail "$desc"
  fi
}

# Shorthand for a --set-string on one fragment. Helm needs the dots in the key
# escaped, otherwise `30-green-alumet.toml` reads as three nested maps.
frag() {
  printf 'config.fragments.%s=%s' "${1//./\\.}" "$2"
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
toml_path = "/etc/perf-sentinel/acks.toml"'

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
toml_path = "/etc/perf-sentinel/acks.toml"'

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

# --- DaemonSet trace splitting ----------------------------------------------
# The guard lives in templates/daemonset.yaml, but `fail` aborts the whole
# render, so --show-only templates/configmap.yaml still observes it.

# 15. DaemonSet without an upstream trace-ID router: a Service round-robins,
#     each pod sees a slice of every trace, and N+1 groups drop under their
#     threshold with nothing to notice it by.
expect_fail "fails on DaemonSet without trace-ID routing" "spanRoutingByTraceId" \
  --set workload.kind=DaemonSet

# 16. Opt-in accepted: the operator asserts the collector routes by trace ID.
expect_render "accepts DaemonSet when trace-ID routing is asserted" 'true' \
  --set workload.kind=DaemonSet --set workload.daemonset.spanRoutingByTraceId=true

# --- Config fragments -------------------------------------------------------
# The daemon reads .perf-sentinel.d/ at startup and hard-fails on a name it
# cannot parse or on two fragments sharing a priority. On a FROM scratch image
# that failure is a CrashLoopBackOff nobody can exec into, so every rule the
# loader enforces is mirrored here (templates/configmap-fragments.yaml).

# 17. A conforming fragment renders into its own ConfigMap.
expect_fragments "renders a conforming fragment" \
  'grep -q "30-green-alumet.toml: |" <<<"$out" &&
   grep -q "endpoint = \"http://alumet:9090/metrics\"" <<<"$out"' \
  --set-string "$(frag 30-green-alumet.toml '[green.alumet]
endpoint = "http://alumet:9090/metrics"')"

# 18. No fragments, no ConfigMap and no mount. Rendered whole rather than with
#     --show-only, which errors out on a template that produces nothing.
if out=$(helm template t "$CHART_DIR" 2>&1) &&
   ! grep -qF "t-perf-sentinel-fragments" <<<"$out" &&
   ! grep -qF "config-fragments" <<<"$out"; then
  report pass "renders no fragments ConfigMap and no mount when unset"
else
  report fail "renders no fragments ConfigMap and no mount when unset"
fi

# 19. Missing NN prefix. The loader rejects it, the chart must too.
expect_fail "fails on a fragment with no NN prefix" "NN-lowercase-name.toml" \
  --set-string "$(frag green-alumet.toml '[green]
enabled = true')"

# 20. One-digit priority: fragment_priority requires exactly two.
expect_fail "fails on a one-digit priority" "NN-lowercase-name.toml" \
  --set-string "$(frag 3-green.toml '[green]
enabled = true')"

# 21. Uppercase in the slug, which the loader's [a-z0-9-] check rejects.
expect_fail "fails on an uppercase fragment name" "NN-lowercase-name.toml" \
  --set-string "$(frag 30-Green.toml '[green]
enabled = true')"

# 22. Two fragments on the same priority: the loader calls the merge order
#     undefined and refuses to start.
expect_fail "fails on a duplicate fragment priority" "duplicate fragment priority 30" \
  --set-string "$(frag 30-green-alumet.toml '[green.alumet]
endpoint = "http://alumet:9090/metrics"')" \
  --set-string "$(frag 30-green-kepler.toml '[green.kepler]
endpoint = "http://kepler:9102/metrics"')"

# 23. A bind port set from a fragment. configmap.yaml cross-checks the ports
#     against the Service reading config.toml only, so this would move the
#     daemon away from where the Service routes with the guard still green.
expect_fail "fails on a listen_port_* set from a fragment" "listen_port_" \
  --set-string "$(frag 30-daemon.toml '[daemon]
listen_port_http = 9999')"

# 24. Same for the tables the chart appends itself under persistence: TOML
#     rejects a table defined twice, so the pod would never parse its config.
#     Persistence is part of the scenario, not incidental to it: that is the
#     only configuration where the chart writes those tables (see 26d).
expect_fail "fails on [daemon.ack] set from a fragment" "daemon.ack" \
  "${PERSIST[@]}" --set-string "$(frag 30-ack.toml '[daemon.ack]
storage_path = "/var/lib/perf-sentinel/acks.jsonl"')"

# 25. Turning green off from a fragment leaves configmap.yaml appending an
#     archive the daemon then refuses to pair with green scoring off.
expect_fail "fails on [green] enabled = false set from a fragment" "green.*enabled = false" \
  --set-string "$(frag 30-green.toml '[green]
enabled = false')"

# 26. The dotted-key spelling opens the same key and a header-only regex
#     misses it.
expect_fail "fails on green.enabled = false set from a fragment" "dotted key or an inline table" \
  --set-string "$(frag 30-green.toml 'green.enabled = false')"

# 26b. TOML allows whitespace inside a table header and quotes around a key
#      name, and an inline table replaces the header entirely. A byte-exact
#      regex sees none of those, so the fragment content is normalised before
#      matching. Each spelling below reached a rendered pod before that.
expect_fail "fails on a spaced [ green ] header" "enabled = false" \
  --set-string "$(frag 30-green.toml '[ green ]
enabled = false')"

expect_fail "fails on the green inline table" "dotted key or an inline table" \
  --set-string "$(frag 30-green.toml 'green = { enabled = false }')"

expect_fail "fails on a listen port in an inline table" "listen_port_" \
  --set-string "$(frag 30-daemon.toml 'daemon = { listen_port_http = 9999 }')"

expect_fail "fails on a quoted listen_port key" "listen_port_" \
  --set-string "$(frag 30-daemon.toml '[daemon]
"listen_port_http" = 9999')"

expect_fail "fails on a spaced [ daemon.ack ] header" "daemon.ack" \
  "${PERSIST[@]}" --set-string "$(frag 30-ack.toml '[ daemon.ack ]
storage_path = "/var/lib/perf-sentinel/acks.jsonl"')"

# 26c. Normalising must not over-fire. A reserved key named in a comment is
#      prose, not config, and comments are stripped before matching.
expect_render "accepts a comment naming a reserved key" 'true' \
  --set-string "$(frag 30-green-kepler.toml '[green.kepler]
# listen_port_http stays in config.toml
endpoint = "http://kepler:9102/metrics"')"

# 26d. The ack/archive guard only exists because the chart appends those tables
#      itself. Without persistence, or with manageDaemonPaths off, the operator
#      owns them and a fragment is a fine place to put them. Firing there would
#      refuse valid input while naming a flag that changes nothing.
expect_render "accepts an archive fragment without persistence" 'true' \
  --set-string "$(frag 30-archive.toml '[daemon.archive]
path = "/tmp/a.ndjson"')"

expect_render "accepts an archive fragment when manageDaemonPaths is off" 'true' \
  "${PERSIST[@]}" --set workload.statefulset.persistence.manageDaemonPaths=false \
  --set-string "$(frag 30-archive.toml '[daemon.archive]
path = "/var/lib/perf-sentinel/archive.ndjson"')"

# 26e. enabled = false is legitimate outside [green]: the guard must target the
#      table it protects, not the key name.
expect_render "accepts enabled = false outside [green]" 'true' \
  --set-string "$(frag 30-correlation.toml '[daemon.correlation]
enabled = false')"

# 27. Fragments reach the pod: the volume, the mount and a checksum covering
#     both ConfigMaps. Without the mount the ConfigMap renders and nothing
#     reads it, which is the failure mode this whole feature exists to avoid.
expect_mounted() {
  local out
  out=$(helm template t "$CHART_DIR" --show-only templates/deployment.yaml "$@" 2>&1) || {
    report fail "mounts the fragments directory (render failed)"; return
  }
  if grep -q "mountPath: /etc/perf-sentinel/.perf-sentinel.d" <<<"$out" &&
     grep -q "name: t-perf-sentinel-fragments" <<<"$out"; then
    report pass "mounts the fragments directory"
  else
    report fail "mounts the fragments directory"
  fi
}
expect_mounted --set-string "$(frag 30-green-alumet.toml '[green.alumet]
endpoint = "http://alumet:9090/metrics"')"

# 28. Editing a fragment must roll the pods. Same config.toml, different
#     fragment: the checksum/config annotation has to move.
#     Captured with `if !` rather than a bare assignment: under `set -e` a
#     failing render would abort the whole harness here, with no FAIL line, no
#     summary, and scenario 29 never reached.
checksum_of() {
  helm template t "$CHART_DIR" --show-only templates/deployment.yaml "$@" 2>&1 |
    grep "checksum/config:"
}
before=""; after=""
if ! before=$(checksum_of --set-string "$(frag 30-green-alumet.toml 'a = 1')"); then
  before=""
fi
if ! after=$(checksum_of --set-string "$(frag 30-green-alumet.toml 'a = 2')"); then
  after=""
fi
if [ -n "$before" ] && [ -n "$after" ] && [ "$before" != "$after" ]; then
  report pass "a fragment edit moves checksum/config"
else
  report fail "a fragment edit moves checksum/config"
fi

# 29. Fragments alongside persistence, the one combination where two features
#     write to the same config. The chart appends [daemon.ack] and
#     [daemon.archive] to config.toml while the fragment mounts separately:
#     both must survive, and the fragment must not have opened those tables.
#     Asserting on the StatefulSet too, not just the ConfigMap: checking only
#     the appended tables would stay green against a chart with the fragments
#     volume and mount deleted, pinning none of the interaction.
expect_render "appends the daemon tables with a fragment mounted" \
  '[ "$(grep -c "^\s*\[daemon\.ack\]" <<<"$out")" = 1 ] &&
   [ "$(grep -c "^\s*\[daemon\.archive\]" <<<"$out")" = 1 ]' \
  "${PERSIST[@]}" --set-string "$(frag 30-green-alumet.toml '[green.alumet]
endpoint = "http://alumet:9090/metrics"')"

# 30. The fragments ConfigMap name must keep its suffix and still tell two
#     releases apart. Suffixing before truncating drops "-fragments" on a long
#     release name and collides with the config ConfigMap; truncating too short
#     makes two releases that differ late share one fragments ConfigMap and
#     overwrite each other's energy backends. Helm caps release names at 53.
fragments_naming() {
  local long out1 out2 names1
  long=$(printf 'b%.0s' $(seq 1 52))
  if ! out1=$(helm template "${long}1" "$CHART_DIR" \
       --set-string "$(frag 30-g.toml '[green]
enabled = true')" 2>&1); then
    report fail "fragments ConfigMap name survives a 53-char release (render failed)"
    return
  fi
  out2=$(helm template "${long}2" "$CHART_DIR" \
       --set-string "$(frag 30-g.toml '[green]
enabled = true')" 2>&1) || true
  names1=$(grep -E "^  name: b" <<<"$out1" | sed 's/^ *name: //' | sort -u)
  if ! grep -q -- "-fragments$" <<<"$names1"; then
    report fail "fragments ConfigMap name keeps its suffix on a long release"
  elif [ "$(wc -l <<<"$names1")" -lt 2 ]; then
    report fail "fragments ConfigMap collides with the config ConfigMap"
  elif [ "$(grep -- '-fragments$' <<<"$names1")" = \
         "$(grep -E '^  name: b' <<<"$out2" | sed 's/^ *name: //' | grep -- '-fragments$')" ]; then
    report fail "two releases share one fragments ConfigMap"
  else
    report pass "fragments ConfigMap name keeps its suffix and stays per-release"
  fi
}
fragments_naming

persist_sts() {
  local out
  if ! out=$(helm template t "$CHART_DIR" --show-only templates/statefulset.yaml \
             "${PERSIST[@]}" --set-string "$(frag 30-green-alumet.toml '[green.alumet]
endpoint = "http://alumet:9090/metrics"')" 2>&1); then
    report fail "mounts fragments and the PVC on the same pod (render failed)"
    return
  fi
  if grep -q "mountPath: /etc/perf-sentinel/.perf-sentinel.d" <<<"$out" &&
     grep -qF "t-perf-sentinel-fragments" <<<"$out" &&
     grep -q "mountPath: /var/lib/perf-sentinel" <<<"$out"; then
    report pass "mounts fragments and the PVC on the same pod"
  else
    report fail "mounts fragments and the PVC on the same pod"
  fi
}
persist_sts

if [ "$failures" -eq 0 ]; then
  echo "All scenarios passed."
  exit 0
fi
echo "$failures scenario(s) failed." >&2
exit 1
