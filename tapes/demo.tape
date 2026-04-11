# VHS script, generates docs/img/analyze/demo.gif and four still frames
# (docs/img/analyze/report-1.png through report-4.png).
# For docs/img/analyze/config.png see the companion tapes/demo-config.tape
# (separate because it uses a much smaller Height).
# For docs/img/explain/* see the companion tapes/demo-explain.tape.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# IMPORTANT: always run `vhs` from the repo root, not from `tapes/`. The
# tape uses paths relative to the current working directory (e.g.
# `tests/fixtures/demo.json`, `docs/img/analyze/demo.gif`) and the
# emulated shell inherits `vhs`'s cwd.
#
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo.tape           # generates analyze/demo.gif (~14 MB raw) + 4 report-*.png
#   vhs tapes/demo-config.tape    # generates analyze/config.png
#   vhs tapes/demo-explain.tape   # generates explain/demo.gif + explain/tree.png
#   rm -f tapes/demo-config-unused.gif
#
#   # Post-process the analyze GIF to cut size from ~14 MB to ~3 MB. The
#   # raw VHS output has too many colors and too high a framerate for a
#   # README asset; ffmpeg with a custom palette handles this cleanly.
#   ffmpeg -y -i docs/img/analyze/demo.gif \
#     -vf "fps=10,scale=800:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
#     -loop 0 /tmp/demo-opt.gif
#   mv /tmp/demo-opt.gif docs/img/analyze/demo.gif

Output docs/img/analyze/demo.gif

Set Shell "bash"
Set FontSize 13
Set Width 1200
Set Height 900
Set TypingSpeed 40ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `mkdir -p /tmp/demo-dir`
Enter
Type `printf '[thresholds]\nio_waste_ratio_max = 0.10\n\n[green]\ndefault_region = "eu-west-3"\n' > /tmp/demo-dir/.perf-sentinel.toml`
Enter
Type `cp tests/fixtures/demo.json /tmp/demo-dir/traces.json`
Enter
Type `cd /tmp/demo-dir && clear`
Enter
Sleep 500ms
Show

Type "cat .perf-sentinel.toml"
Sleep 500ms
Enter
Sleep 3s

Type "perf-sentinel analyze --input traces.json"
Sleep 1s
Enter

# Output ends with `Quality gate: FAILED` at the bottom of the viewport.
# Pause briefly so the viewer catches the red line, then rewind to the
# top of the report and walk through the 10 findings page by page. The
# four screenshots below populate the README's still-frames section.
#
# Note: the last page is captured FIRST (before ScrollUp) so the Quality
# gate line is guaranteed inside the viewport. Subsequent pages use a
# ~40-row `ScrollDown` stride with ~8-row overlap: that overlap ensures
# every finding appears fully on at least one page (any finding partially
# cut at the bottom of page N reappears whole at the top of page N+1).
Sleep 2s
Screenshot docs/img/analyze/report-4.png
Sleep 2s

ScrollUp 300
Sleep 1s
Screenshot docs/img/analyze/report-1.png
Sleep 2s

ScrollDown 40
Sleep 1s
Screenshot docs/img/analyze/report-2.png
Sleep 2s

ScrollDown 40
Sleep 1s
Screenshot docs/img/analyze/report-3.png
Sleep 2s
