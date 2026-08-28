# VHS script, generates docs/img/inspect/demo.gif and five still frames
# (main.png, detail.png, detail-scrolled.png, info.png).
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
# The screenshots still cover all three severity levels plus a rich
# detail-panel view with scrolling. Positions below are in the impact
# ranking the list now opens on, not the trace-id order it used to:
#   - main.png           : position 0, the costliest trace
#                          (N+1 SQL, CRITICAL red)
#   - detail.png         : position 6 (fanout, WARNING yellow) with the
#                          detail panel active via Tab Tab,
#                          scroll_offset = 0 (top of tree)
#   - detail-scrolled.png: same trace, after `Down` × 15 in the detail
#                          panel - viewport has scrolled through the
#                          fanout span tree to reveal its bottom half
#   - info.png           : position 3 (redundant HTTP, INFO cyan)
#
# There is no separate CRITICAL frame: impact ordering opens the list on
# the one critical finding the demo fixture holds, so main.png already is
# that frame. Capturing it twice produced two byte-identical files.
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
#   # fps=20 keeps motion smooth; the filter lands it around 1.7 MB while
#   # keeping text readable.
#   ffmpeg -y -i docs/img/inspect/demo.gif \
#     -vf "fps=20,scale=1200:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
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

# The list opens on the highest-impact trace, so the walk down is now a
# walk down the impact ranking. Six steps reach the fanout trace, whose
# full 22-span tree of `POST /api/checkout/confirm` is the most visually
# rich in the demo fixture, then Tab Tab drills into the detail panel.
Down@600ms 6
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
# the 4-panel layout). Then walk back up three places to the INFO trace
# (redundant HTTP). `move_up` on Traces resets `scroll_offset` and
# clears `cached_detail` just as `move_down` does, so the scrolled state
# does not leak into the next trace's view. The findings panel header
# flips to cyan INFO as the viewer watches the color shift.
Tab
Sleep 1s
Tab
Sleep 3s
Up@600ms 3
Sleep 4s
Screenshot docs/img/inspect/info.png
Sleep 4s

Type "q"
