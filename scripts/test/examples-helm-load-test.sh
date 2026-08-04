#!/usr/bin/env bash
#
# Boot-test the published Helm examples.
#
# Usage:
#   scripts/test/examples-helm-load-test.sh [path/to/perf-sentinel]
#
# Called by .github/workflows/helm-ci.yml, also runnable locally. Defaults to
# target/debug/perf-sentinel, so `cargo build --bin perf-sentinel` first.
#
# `helm template` proving a values file renders says nothing about whether the
# daemon accepts what came out: a fragment that parses but does not validate
# renders fine and CrashLoopBackOffs on a FROM scratch image with no shell to
# read the error in. So this renders both ConfigMaps for every
# values-green-*.yaml overlay, projects them the way kubelet does (real files
# under a timestamped directory, a `..data` symlink, one relative symlink per
# key) and loads the result with the real binary.
#
# The symlink layout is the part a render can never prove: the loader walks
# .perf-sentinel.d/ with read_dir and has to skip `..data` and the timestamped
# directory on its own.
#
# What this does NOT catch: a `[green.*]` section missing the `endpoint` that
# activates it converts to None during raw-to-typed conversion, so no validation
# runs and the load succeeds. Dropping `metric_name` from an active backend is
# caught, dropping `endpoint` is not. The parity check at the end covers the
# missing-field case that this cannot.
#
# Exit codes:
#   0 - every example loads
#   1 - an example failed, or helm / the binary is missing

set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHART="$REPO/charts/perf-sentinel"
BASE="$REPO/examples/helm/values-perf-sentinel.yaml"
FIXTURE="$REPO/tests/fixtures/clean_traces.json"
BIN="${1:-$REPO/target/debug/perf-sentinel}"
# Fixed, because kubelet names this directory after the projection timestamp
# and the loader must skip it whatever it is called.
STAMP="..2026_08_04_13_00_00.123456789"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
failures=0

command -v helm >/dev/null || { echo "error: helm not found" >&2; exit 1; }
[ -x "$BIN" ] || { echo "error: $BIN not found, run cargo build --bin perf-sentinel" >&2; exit 1; }

# Rebuild one ConfigMap's keys as files under $2.
split_configmap() {
  python3 -c '
import pathlib, re, sys
body = pathlib.Path(sys.argv[1]).read_text().split("data:\n", 1)[1]
target = pathlib.Path(sys.argv[2])
target.mkdir(parents=True, exist_ok=True)
for m in re.finditer(r"^  ([\w.\-]+): \|\n((?:    .*\n|\n)*)", body, re.M):
    content = "".join(l[4:] if l.startswith("    ") else l
                      for l in m.group(2).splitlines(True))
    (target / m.group(1)).write_text(content)
' "$1" "$2"
}

# The base values on their own, then each green overlay stacked on it. The
# base must load by itself: it is what the README tells operators to install
# first, and every overlay inherits whatever it gets wrong.
for overlay in "" "$REPO"/examples/helm/values-green-*.yaml; do
  if [ -z "$overlay" ]; then
    name="values-perf-sentinel.yaml (base)"
    args=(-f "$BASE")
  else
    name=$(basename "$overlay")
    args=(-f "$BASE" -f "$overlay")
  fi

  mnt="$WORK/$name/etc/perf-sentinel"
  frag_dir="$mnt/.perf-sentinel.d"
  mkdir -p "$frag_dir/$STAMP"

  if ! helm template t "$CHART" "${args[@]}" -s templates/configmap.yaml \
       > "$WORK/main.yaml" 2>&1; then
    echo "FAIL $name (config render): $(head -1 "$WORK/main.yaml")"
    failures=$((failures + 1)); continue
  fi
  sed -n '/perf-sentinel.toml: |/,$p' "$WORK/main.yaml" | tail -n +2 | sed 's/^    //' \
    > "$mnt/.perf-sentinel.toml"
  # The extraction is a sed range over a rendered template, so a template
  # reshuffle can silently produce nothing. `analyze` happily runs on defaults
  # and prints a full report, so without this the whole harness would go green
  # while loading none of the chart's config.
  if [ ! -s "$mnt/.perf-sentinel.toml" ]; then
    echo "FAIL $name (extracted config.toml is empty, the ConfigMap shape changed)"
    failures=$((failures + 1)); continue
  fi

  # --show-only errors out on a template that renders nothing, which is the
  # correct output when an example declares no fragments. Count what the
  # example declares so a projection that writes nothing is caught below,
  # rather than reported as a pass over config.toml alone.
  want=0
  if helm template t "$CHART" "${args[@]}" -s templates/configmap-fragments.yaml \
     > "$WORK/frag.yaml" 2>&1; then
    if ! split_configmap "$WORK/frag.yaml" "$frag_dir/$STAMP"; then
      echo "FAIL $name (projecting the fragments ConfigMap failed)"
      failures=$((failures + 1)); continue
    fi
    want=$(grep -cE '^  [0-9]{2}-[a-z0-9-]+\.toml: \|$' "$WORK/frag.yaml")
  fi
  got=$(find "$frag_dir/$STAMP" -name '*.toml' -type f | wc -l | tr -d ' ')
  if [ "$want" != "$got" ]; then
    echo "FAIL $name (declares $want fragment(s), projected $got)"
    failures=$((failures + 1)); continue
  fi

  (cd "$frag_dir" && ln -s "$STAMP" ..data &&
   for f in "$STAMP"/*.toml; do
     [ -e "$f" ] && ln -s "..data/$(basename "$f")" "$(basename "$f")"
   done) 2>/dev/null

  # Only the Electricity Maps overlay expects a token. Exporting it for every
  # run would activate that backend on overlays that declare no region_map,
  # which the daemon rejects, and the failure would look like theirs.
  if [[ "$name" == *electricity-maps* ]]; then
    out=$(PERF_SENTINEL_EMAPS_TOKEN=dummy "$BIN" analyze \
      --input "$FIXTURE" --config "$mnt/.perf-sentinel.toml" 2>&1)
  else
    out=$("$BIN" analyze --input "$FIXTURE" --config "$mnt/.perf-sentinel.toml" 2>&1)
  fi

  # Assert a report came out, rather than only that no line said "Error": a
  # binary that never ran leaves $out empty and would read as a pass.
  if grep -qE "^Error" <<<"$out" || ! grep -q "Quality gate:" <<<"$out"; then
    echo "FAIL $name"
    grep -E "^Error" <<<"$out" | head -2 | sed 's/^/       /'
    [ -z "$out" ] && echo "       (no output at all)"
    failures=$((failures + 1))
  else
    loaded=$(ls "$frag_dir/$STAMP" 2>/dev/null | tr '\n' ' ')
    echo "PASS $name  [fragments: ${loaded:-none}]"
  fi
done

# --- The loader really reads the projected directory -------------------------
#
# Everything above proves the examples load. None of it proves the fragments
# were part of that load: config.toml alone produces the same successful report,
# so a projection that silently wrote nothing, or a mount path the daemon never
# looks at, would read as a pass.
#
# So plant a fragment that cannot parse into the same kubelet-shaped directory
# and require the binary to reject it *by name*. Only a loader that opened
# .perf-sentinel.d/ can produce that message.
#
# A syntax error, not an unknown key: an unrecognised field inside a
# [green.*] table is accepted silently, so it would prove nothing.
probe="$WORK/probe/etc/perf-sentinel"
mkdir -p "$probe/.perf-sentinel.d/$STAMP"
helm template t "$CHART" -f "$BASE" -s templates/configmap.yaml > "$WORK/probe-main.yaml" 2>&1
sed -n '/perf-sentinel.toml: |/,$p' "$WORK/probe-main.yaml" | tail -n +2 | sed 's/^    //' \
  > "$probe/.perf-sentinel.toml"
printf '[green.kepler\nendpoint = "http://k:9102/metrics"\n' \
  > "$probe/.perf-sentinel.d/$STAMP/50-canary.toml"
(cd "$probe/.perf-sentinel.d" && ln -s "$STAMP" ..data &&
 ln -s "..data/50-canary.toml" 50-canary.toml)

out=$("$BIN" analyze --input "$FIXTURE" --config "$probe/.perf-sentinel.toml" 2>&1)
if grep -q "50-canary.toml" <<<"$out"; then
  echo "PASS the daemon reads .perf-sentinel.d/ through the kubelet symlink layout"
else
  echo "FAIL the daemon never read .perf-sentinel.d/: a broken fragment planted there"
  echo "     was not rejected, so every PASS above covers config.toml only."
  echo "     got: $(head -2 <<<"$out")"
  failures=$((failures + 1))
fi

# --- Field parity with the examples/ fragments -------------------------------
#
# Each values-green-*.yaml is the Kubernetes port of the examples/NN-*.toml of
# the same name, and the pair drifts silently: the .toml gains a key, the
# overlay does not, and nobody notices until an operator copies the overlay and
# wonders where the setting went. That is how examples/helm/ fell four months
# behind examples/ before this script existed.
#
# So every key and table the .toml mentions, set or commented, must appear in
# the overlay. Values are free to differ, and have to: localhost becomes
# in-cluster DNS. Only the field has to survive the port.
#
# Exempt, and each overlay says why in its header:
#   [green], enabled, default_region  the base values set them in config.toml,
#                                     merged after the fragment, so a copy here
#                                     would be silently overridden
#   api_key                           a fragment renders into a ConfigMap; the
#                                     token goes through a Secret instead
EXEMPT='^(\[green\]|enabled|default_region|api_key)$'

mentioned() {
  # Digits belong in the class: a table such as [green.cloud.services."api-us2"]
  # would otherwise be invisible on both sides and drop out of the diff.
  grep -oE '^[[:space:]]*#?[[:space:]]*(\[[a-z0-9._"|-]+\]|[a-z_][a-z0-9_]* *=)' "$1" \
    | sed 's/^[[:space:]]*//; s/^# *//; s/ *=$//; s/[[:space:]]*$//' \
    | grep -v '^$' | sort -u
}

for toml in "$REPO"/examples/[0-9][0-9]-green-*.toml; do
  backend=$(basename "$toml" .toml | sed 's/^[0-9]*-green-//')
  overlay="$REPO/examples/helm/values-green-$backend.yaml"
  if [ ! -f "$overlay" ]; then
    echo "FAIL $(basename "$toml") has no values-green-$backend.yaml overlay"
    failures=$((failures + 1)); continue
  fi
  missing=$(comm -23 <(mentioned "$toml") <(mentioned "$overlay") | grep -Ev "$EXEMPT")
  if [ -n "$missing" ]; then
    echo "FAIL $(basename "$overlay") drops fields carried by $(basename "$toml"):"
    sed 's/^/         /' <<<"$missing"
    failures=$((failures + 1))
  else
    echo "PASS $(basename "$overlay") carries every field of $(basename "$toml")"
  fi
done

if [ "$failures" -eq 0 ]; then
  echo "Every Helm example loads and mirrors its examples/ fragment."
  exit 0
fi
echo "$failures example(s) failed." >&2
exit 1
