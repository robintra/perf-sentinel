# VHS script, generates docs/img/monitor/advisor.png only (the companion
# GIF is unused, gitignored under /tapes/demo-monitor-advisor-unused.gif).
#
# Captures the Advisor tab of `perf-sentinel query monitor` (since 0.8.8)
# in the state where it has something to say: an undersized daemon. The
# nominal daemon in demo-monitor.tape reports "No hints", which is the
# healthy state but does not illustrate the settings advisor. Here a
# small [daemon] max_active_traces plus a long trace_ttl_ms keeps the
# TraceWindow pinned at the cap, so the daemon emits a real `tuning`
# hint: "active traces are within 90% of max_active_traces". The
# evicted traces are still scored, so green data exists, but only the
# Advisor tab is captured.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-monitor-advisor.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-monitor-advisor.tape
#   python3 scripts/trim-bottom-png.py docs/img/monitor/advisor.png

Output tapes/demo-monitor-advisor-unused.gif

Set Shell "bash"
Set FontSize 13
Set Width 1600
# The Advisor tab holds only a title, a one-line description and the
# hint, so a short viewport avoids a near-empty capture.
Set Height 200
Set TypingSpeed 100ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `export RUST_LOG=error`
Enter
Type `mkdir -p /tmp/demo-monitor-adv-dir && cd /tmp/demo-monitor-adv-dir && rm -f perf-sentinel.sock`
Enter
# Undersized daemon: a small max_active_traces with a long trace_ttl_ms
# keeps the LRU window pinned at the cap, so active_traces stays above
# the 90% advisor threshold and the tuning hint fires and stays. Green is
# still enabled (evicted traces are scored) but only Advisor is shown.
Type `printf '[daemon]\njson_socket = "/tmp/demo-monitor-adv-dir/perf-sentinel.sock"\ntrace_ttl_ms = 60000\nmax_active_traces = 5\n\n[green]\nenabled = true\ndefault_region = "eu-west-3"\n' > .perf-sentinel.toml`
Enter
Type `perf-sentinel watch --config .perf-sentinel.toml --listen-port-http 14318 --listen-port-grpc 14319 > /tmp/demo-monitor-adv-daemon.log 2>&1 &`
Enter
Type `sleep 2`
Enter
Type `python3 -c "import socket, json; e=json.load(open('$OLDPWD/tests/fixtures/demo.json')); s=socket.socket(socket.AF_UNIX); s.connect('/tmp/demo-monitor-adv-dir/perf-sentinel.sock'); s.sendall((json.dumps(e)+chr(10)).encode()); s.shutdown(socket.SHUT_WR)"`
Enter
Type `sleep 2 && clear`
Enter
Sleep 1s
Show

Type "perf-sentinel query --daemon http://127.0.0.1:14318 monitor --refresh 3"
Sleep 800ms
Enter
# Opens on the Advisor tab, which now carries the tuning hint.
Sleep 3s
Screenshot docs/img/monitor/advisor.png
Sleep 1500ms

Type "q"
Sleep 500ms

Hide
Type "kill %1 2> /dev/null; wait 2> /dev/null"
Enter
Sleep 300ms
