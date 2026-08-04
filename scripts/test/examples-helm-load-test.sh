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
# daemon accepts what came out: a fragment missing a required key renders fine
# and CrashLoopBackOffs on a FROM scratch image with no shell to read the error
# in. So this renders both ConfigMaps for every values-green-*.yaml overlay,
# projects them the way kubelet does (real files under a timestamped directory,
# a `..data` symlink, one relative symlink per key) and loads the result with
# the real binary.
#
# The symlink layout is the part a render can never prove: the loader walks
# .perf-sentinel.d/ with read_dir and has to skip `..data` and the timestamped
# directory on its own.
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

  # --show-only errors out on a template that renders nothing, which is the
  # correct output when an example declares no fragments.
  if helm template t "$CHART" "${args[@]}" -s templates/configmap-fragments.yaml \
     > "$WORK/frag.yaml" 2>&1; then
    split_configmap "$WORK/frag.yaml" "$frag_dir/$STAMP"
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

if [ "$failures" -eq 0 ]; then
  echo "Every Helm example loads."
  exit 0
fi
echo "$failures example(s) failed." >&2
exit 1
