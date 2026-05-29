# VHS script, generates docs/img/tui/demo.gif and three still frames
# (analyze.png, inspect.png, explain.png).
#
# Showcases the all-in-one interactive TUI: the three views (Analyze,
# Inspect, Explain) navigated as a single drill-down without leaving the
# terminal. `analyze --tui` opens on the Analyze summary, Enter descends
# to the Inspect 4-panel browser, then through the panels into the
# full-screen Explain span tree, and Esc walks all the way back up. The
# tab bar at the top tracks the active view throughout.
#
# Navigation, with the demo fixture (traces sorted by trace_id, so the
# positions are deterministic):
#   - analyze.png : the Analyze view (GreenOps summary + quality gate)
#   - Enter       : descend to Inspect, Traces panel at position 0
#   - inspect.png : the 4-panel Inspect browser
#   - Down        : position 1 (fanout, the richest span tree)
#   - Enter x3    : Traces -> Findings -> Detail -> Explain
#   - explain.png : the fanout trace's full-screen span tree
#   - Esc x4      : Explain -> Detail -> Findings -> Traces -> Analyze
#
# RUST_LOG=error keeps the alternate screen clean (no info logs leak into
# the TUI render).
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-tui.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-tui.tape   # generates tui/demo.gif + 3 *.png
#
#   # Post-process the GIF to keep it well under ~1 MB. Keep fps=20 (do not
#   # decimate) so the typed command stays smooth letter-by-letter:
#   ffmpeg -y -i docs/img/tui/demo.gif \
#     -vf "fps=20,scale=1100:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
#     -loop 0 /tmp/demo-tui-opt.gif
#   mv /tmp/demo-tui-opt.gif docs/img/tui/demo.gif

Output docs/img/tui/demo.gif

Set Shell "bash"
Set FontSize 13
Set Width 1600
Set Height 900
# 100ms typing keeps the launch command at roughly one character per output
# frame (natural letter-by-letter); the post-process below keeps fps=20 so
# it does not turn blocky.
Set TypingSpeed 100ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `export RUST_LOG=error`
Enter
Type `mkdir -p /tmp/demo-tui-dir`
Enter
Type `cp tests/fixtures/demo.json /tmp/demo-tui-dir/traces.json`
Enter
Type `cd /tmp/demo-tui-dir && clear`
Enter
Sleep 500ms
Show

Sleep 1s
Type "perf-sentinel analyze --tui --input traces.json"
Sleep 1s
Enter
Sleep 3s
Screenshot docs/img/tui/analyze.png
Sleep 3s

# Enter descends to the Inspect 4-panel browser.
Enter
Sleep 3s
Screenshot docs/img/tui/inspect.png
Sleep 3s

# Walk to the fanout trace (position 1) and drill all the way to Explain:
# Traces -> Findings -> Detail -> Explain.
Down
Sleep 2s
Enter
Sleep 2s
Enter
Sleep 2s
Enter
Sleep 3s
Screenshot docs/img/tui/explain.png
Sleep 3s

# Esc walks back up: Explain -> Detail -> Findings -> Traces -> Analyze.
Escape
Sleep 1500ms
Escape
Sleep 1500ms
Escape
Sleep 1500ms
Escape
Sleep 2s

Type "q"
