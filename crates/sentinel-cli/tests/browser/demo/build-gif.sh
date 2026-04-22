#!/usr/bin/env bash
set -euo pipefail

# Convert each WebM recorded by Playwright (one per project) into a
# palette-optimised GIF under docs/img/report/. Run via `npm run demo`
# from crates/sentinel-cli/tests/browser.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BROWSER_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$BROWSER_DIR/../../../.." && pwd)"
OUT_DIR="$REPO_ROOT/docs/img/report"

mkdir -p "$OUT_DIR"

# 15 fps keeps cursor motion smooth without bloating the GIF. The
# palette split avoids the 256-colour cliff on gradient-heavy tabs.
# max_colors=96 plus Bayer dithering lands each file around 2-3 MB
# for a ~25 s recording at 1000 px wide.
convert_webm() {
  local webm="$1"
  local out="$2"
  echo "build-gif: $webm -> $out"
  ffmpeg -y -i "$webm" -vf \
    "fps=15,scale=1000:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=96[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
    -loop 0 "$out" 2> /tmp/build-gif.log
  local sz
  sz="$(du -k "$out" | awk '{print $1}')"
  echo "build-gif: wrote $out (${sz} KB)"
}

shopt -s nullglob
found=0
for webm in "$BROWSER_DIR"/demo-videos/*/video.webm; do
  found=1
  dir="$(basename "$(dirname "$webm")")"
  case "$dir" in
    *dashboard-dark*)  out_name="dashboard_dark.gif"  ;;
    *dashboard-light*) out_name="dashboard_light.gif" ;;
    *)
      echo "build-gif: cannot map project for $webm" >&2
      exit 1
      ;;
  esac
  convert_webm "$webm" "$OUT_DIR/$out_name"
done

if [[ "$found" -eq 0 ]]; then
  echo "build-gif: no webm found under $BROWSER_DIR/demo-videos/" >&2
  exit 1
fi
