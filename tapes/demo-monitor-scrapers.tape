# VHS script, generates docs/img/monitor/scrapers.png only (the companion
# GIF is unused, gitignored under /tapes/demo-monitor-scrapers-unused.gif).
#
# Captures the Scrapers tab of `perf-sentinel query monitor` (since
# 0.8.8) at a height sized to its content, instead of the taller
# demo-monitor.tape viewport (which is sized for the fullest tab,
# Energy). The Scrapers tab only reads /api/energy, so no trace seeding
# is needed: a [green.cloud] backend pointed at an unreachable loopback
# endpoint shows configured=yes with a freshness age that climbs, while
# the unconfigured backends stay dashed. Offline-safe (no real backend,
# no external network destination).
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-monitor-scrapers.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-monitor-scrapers.tape
#   python3 scripts/trim-bottom-png.py docs/img/monitor/scrapers.png

Output tapes/demo-monitor-scrapers-unused.gif

Set Shell "bash"
Set FontSize 13
# Sized for the Scrapers tab content: 700 px wide fits the widest line
# (the top header row of tabs + daemon URL) with a small margin, 320 px
# tall fits a header row, five backend rows and the two-line
# electricity_maps note plus the border.
Set Width 700
Set Height 320
Set TypingSpeed 100ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `export RUST_LOG=error`
Enter
Type `mkdir -p /tmp/demo-monitor-scr-dir && cd /tmp/demo-monitor-scr-dir && rm -f perf-sentinel.sock`
Enter
# Minimal daemon: green enabled with a single [green.cloud] backend
# pointed at an unreachable loopback endpoint. No trace seeding, the
# Scrapers tab reads only /api/energy.
Type `printf '[daemon]\njson_socket = "/tmp/demo-monitor-scr-dir/perf-sentinel.sock"\n\n[green]\nenabled = true\n\n[green.cloud]\nprometheus_endpoint = "http://127.0.0.1:19999/metrics"\nscrape_interval_secs = 2\n\n[green.cloud.services]\norder-svc = { idle_watts = 5.0, max_watts = 30.0 }\n' > .perf-sentinel.toml`
Enter
Type `perf-sentinel watch --config .perf-sentinel.toml --listen-port-http 14318 --listen-port-grpc 14319 > /tmp/demo-monitor-scr-daemon.log 2>&1 &`
Enter
# Let several cloud scrape attempts fail so the freshness age is visibly
# non-zero by the time the Scrapers tab is shown.
Type `sleep 8 && clear`
Enter
Sleep 1s
Show

Type "perf-sentinel query --daemon http://127.0.0.1:14318 monitor --refresh 3"
Sleep 800ms
Enter
# Opens on Advisor; Tab twice to reach Scrapers (Advisor -> Energy -> Scrapers).
Sleep 1500ms
Tab
Sleep 1200ms
Tab
Sleep 2500ms
Screenshot docs/img/monitor/scrapers.png
Sleep 1500ms

Type "q"
Sleep 500ms

Hide
Type "kill %1 2> /dev/null; wait 2> /dev/null"
Enter
Sleep 300ms
