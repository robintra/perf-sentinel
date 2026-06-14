# VHS script, generates docs/img/monitor/trends.png only (the companion
# GIF is unused, gitignored under /tapes/demo-monitor-trends-unused.gif).
#
# Captures the Trends tab of `perf-sentinel query monitor` (since 0.8.8)
# once its braille charts have filled the fixed plotting window, so the
# curves span the whole -Nm..now axis instead of hugging the right edge.
# The window is TREND_CAPACITY (240) points wide and one point lands per
# --refresh tick, so at --refresh 1 it takes ~4 min of polling to fill.
# That is far too long for the demo-monitor.tape tour GIF, which is why
# this still is captured on its own tape (same pattern as the Advisor and
# Scrapers stills).
#
# Seeding mirrors demo-monitor.tape: a [daemon] json_socket takes the
# native fixture directly, a short trace_ttl_ms flushes the TraceWindow
# so green scoring runs, and scripts/demo-seed-loop.py keeps pushing
# randomly-sized sub-batches so the energy/carbon curves and the headroom
# gauges actually move tick after tick. [green.cloud] points at an
# unreachable loopback endpoint (offline-safe, no external destination).
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-monitor-trends.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-monitor-trends.tape   # ~4 min to fill the window
#   python3 scripts/trim-bottom-png.py docs/img/monitor/trends.png

Output tapes/demo-monitor-trends-unused.gif

Set Shell "bash"
Set FontSize 13
# Same viewport as demo-monitor.tape so the still matches the other tabs:
# 810 cols, 600 rows gives the two Trends chart rows ~12 lines each.
Set Width 810
Set Height 600
Set TypingSpeed 100ms
Set Padding 15
# The companion GIF is discarded, so capture few frames to keep the
# throwaway file and the encode cheap over the ~4 min fill.
Set Framerate 4
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `export RUST_LOG=error`
Enter
Type `mkdir -p /tmp/demo-monitor-trends-dir && cd /tmp/demo-monitor-trends-dir && rm -f perf-sentinel.sock`
Enter
Type `printf '[daemon]\njson_socket = "/tmp/demo-monitor-trends-dir/perf-sentinel.sock"\ntrace_ttl_ms = 400\nmax_active_traces = 60\nmax_retained_findings = 500\n\n[green]\nenabled = true\ndefault_region = "eu-west-3"\n\n[green.service_regions]\ncheckout-svc = "us-east-1"\ngateway-svc = "eu-west-1"\ndashboard-svc = "ap-southeast-2"\nreports-svc = "us-west-1"\n\n[green.cloud]\nprometheus_endpoint = "http://127.0.0.1:19999/metrics"\nscrape_interval_secs = 2\n\n[green.cloud.services]\norder-svc = { idle_watts = 5.0, max_watts = 30.0 }\n' > .perf-sentinel.toml`
Enter
Type `perf-sentinel watch --config .perf-sentinel.toml --listen-port-http 14318 --listen-port-grpc 14319 > /tmp/demo-monitor-trends-daemon.log 2>&1 &`
Enter
Type `sleep 2`
Enter
# One-shot seed for the green tables, then the background loop that keeps
# the Trends curves moving (random sub-batches, unique trace ids).
Type `python3 -c "import socket, json; e=json.load(open('$OLDPWD/tests/fixtures/demo.json')); s=socket.socket(socket.AF_UNIX); s.connect('/tmp/demo-monitor-trends-dir/perf-sentinel.sock'); s.sendall((json.dumps(e)+chr(10)).encode()); s.shutdown(socket.SHUT_WR)"`
Enter
Type `python3 "$OLDPWD/scripts/demo-seed-loop.py" /tmp/demo-monitor-trends-dir/perf-sentinel.sock "$OLDPWD/tests/fixtures/demo.json" > /dev/null 2>&1 &`
Enter
Type `sleep 3 && clear`
Enter
Sleep 1s
Show

# Open the monitor and Tab straight to Trends; the poller fills the window
# from empty at one point per second.
Type "perf-sentinel query --daemon http://127.0.0.1:14318 monitor --refresh 1"
Sleep 800ms
Enter
Sleep 1500ms
# Advisor -> Energy -> Trends.
Tab
Sleep 600ms
Tab
Sleep 600ms
# Fill the 240-point window: 250 s of polling overfills it slightly so the
# curves span the full -Nm..now axis with no empty left margin.
Sleep 250s
Screenshot docs/img/monitor/trends.png
Sleep 1500ms

Type "q"
Sleep 500ms

Hide
Type "kill %1 %2 2> /dev/null; wait 2> /dev/null"
Enter
Sleep 300ms
