# VHS script, generates docs/img/inspect/report-mode.png.
#
# Showcases `perf-sentinel inspect --input <Report.json>` (Phase 8.2
# Report-shape auto-detection extended to the inspect subcommand). The
# tape feeds the binary a pre-computed Report carrying findings and
# cross-trace correlations (which the daemon would normally produce),
# then drills into the Detail panel to surface the "span tree not
# available" hint that points the viewer at the two paths that DO
# carry raw spans (`inspect --input <events>.json`, `query inspect`
# against a daemon).
#
# Companion to tapes/demo-inspect.tape, which covers the events-mode
# happy path with the full 4-panel interactive tour.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-inspect-report.tape`.
# Run `python3 scripts/trim-bottom-png.py docs/img/inspect/report-mode.png`
# afterwards to drop the empty terminal background below the Detail
# panel. The GIF below is a VHS-mandatory side artefact (every tape
# needs an `Output`), gitignored under `/tapes/demo-inspect-report-unused.gif`
# in the same way `demo-config.tape` ships its config.png alongside an
# unused .gif. This tape only ships the PNG.

Output tapes/demo-inspect-report-unused.gif

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
Type `mkdir -p /tmp/demo-inspect-report-dir`
Enter
Type `cp tests/fixtures/report_with_correlations.json /tmp/demo-inspect-report-dir/snapshot.json`
Enter
Type `cd /tmp/demo-inspect-report-dir && clear`
Enter
Sleep 500ms
Show

Sleep 1s
Type "perf-sentinel inspect --input snapshot.json"
Sleep 1500ms
Enter
Sleep 3s

# Drill into the Detail panel: Tab Tab moves Traces -> Findings -> Detail.
# The Findings panel auto-selects the first finding when entered, so the
# Detail panel renders the metadata block + the span-tree hint (Reports
# don't ship raw spans, so the tree is intentionally blank with guidance).
Tab
Sleep 1500ms
Tab
Sleep 2s
Screenshot docs/img/inspect/report-mode.png
Sleep 3s

Type "q"
