# VHS script, generates docs/img/inspect/demo.gif and five still frames
# (main.png, detail.png, detail-scrolled.png, critical.png, info.png).
#
# Showcases the `perf-sentinel inspect` TUI mode, a 4-panel interactive
# explorer for traces, findings, finding details and cross-trace
# correlations. The Correlations panel stays empty in batch mode (the
# correlator is daemon-only), the tape exercises the other three. The
# tape launches
# the TUI, navigates down through the trace list one position at a time
# with ~2s pauses so the viewer can watch the findings panel color flip
# between severity levels (yellow WARNING → red CRITICAL → cyan INFO),
# then Tabs into the detail panel and quits with 'q'.
#
# The trace list is sorted alphabetically by `trace_id` in `App::new`,
# so the navigation below is deterministic across runs. With the demo
# fixture, positions are:
#   0: chatty         (WARNING)
#   1: fanout         (WARNING, 22 spans - good detail view)
#   2: nplus-http     (WARNING)
#   3: nplus-sql      (CRITICAL, 10 occurrences)
#   4: pool           (WARNING)
#   5: redundant-http (INFO)
#   6: redundant-sql  (INFO)
#   7: serialized     (INFO)
#   8: slow-http      (WARNING)
#   9: slow-sql       (WARNING)
#
# The screenshots land on finding types that cover all three severity
# levels plus a rich detail-panel view with scrolling:
#   - main.png           : position 0 (chatty, WARNING)
#   - detail.png         : position 1 (fanout) with detail panel active
#                          via Tab Tab, scroll_offset = 0 (top of tree)
#   - detail-scrolled.png: same trace, after `Down` × 15 in the detail
#                          panel - viewport has scrolled through the
#                          fanout span tree to reveal its bottom half
#   - critical.png       : position 3 (N+1 SQL, CRITICAL red)
#   - info.png           : position 5 (redundant HTTP, INFO cyan)
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-inspect.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-inspect.tape   # generates inspect/demo.gif + 5 *.png
#
#   # Post-process the GIF. Required: the raw VHS output grows past
#   # 1.5 MB because the tape is long (3 severity tours + a scroll
#   # animation) and the full-width terminal has lots of varied pixels.
#   # The filter below cuts it to ~950 KB while keeping text readable.
#   ffmpeg -y -i docs/img/inspect/demo.gif \
#     -vf "fps=10,scale=1200:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
#     -loop 0 /tmp/demo-inspect-opt.gif
#   mv /tmp/demo-inspect-opt.gif docs/img/inspect/demo.gif

Output docs/img/inspect/demo.gif

Set Shell "bash"
Set FontSize 13
Set Width 1600
Set Height 900
Set TypingSpeed 40ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `mkdir -p /tmp/demo-inspect-dir`
Enter
Type `cp tests/fixtures/demo.json /tmp/demo-inspect-dir/traces.json`
Enter
Type `cd /tmp/demo-inspect-dir && clear`
Enter
Sleep 500ms
Show

Sleep 1s
Type "perf-sentinel inspect --input traces.json"
Sleep 1500ms
Enter
Sleep 3s
Screenshot docs/img/inspect/main.png
Sleep 4s

# Step down to the fanout trace (position 1) and drill into the detail
# panel via Tab Tab. Detail panel shows the full 22-span tree of the
# `POST /api/checkout/confirm` trace, the most visually rich span tree
# in the demo fixture.
Down
Sleep 4s
Tab
Sleep 3s
Tab
Sleep 4s
Screenshot docs/img/inspect/detail.png
Sleep 3s

# Demonstrate scrolling within the Detail panel. Each `Down` keystroke
# while the Detail panel is active increments `scroll_offset` by 1 row.
# The `@400ms` modifier spaces the 15 presses out so the viewer sees
# the viewport walk through the span tree one line at a time rather
# than a single instant jump.
Down@400ms 15
Sleep 2s
Screenshot docs/img/inspect/detail-scrolled.png
Sleep 4s

# Rewind back to the traces panel. Two Tabs because the cycle is now
# Detail → Correlations → Traces (the Correlations panel was added in
# the 4-panel layout). Then walk down to the CRITICAL trace (N+1 SQL
# ×10 at position 3). `move_down` on Traces automatically resets
# `scroll_offset` and clears `cached_detail`, so the scrolled state
# does not leak into the next trace's view. The findings panel
# header flips from yellow WARNING to red CRITICAL as the viewer
# watches the color shift.
Tab
Sleep 1s
Tab
Sleep 3s
Down
Sleep 4s
Down
Sleep 4s
Screenshot docs/img/inspect/critical.png
Sleep 4s

# Keep walking down to the INFO trace (redundant HTTP at position 5).
# Findings panel header flips to cyan INFO.
Down
Sleep 4s
Down
Sleep 4s
Screenshot docs/img/inspect/info.png
Sleep 4s

Type "q"
