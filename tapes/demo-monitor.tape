# VHS script, generates docs/img/monitor/demo.gif plus the energy.png,
# config.png and incidents.png still frames. The Trends still is captured
# separately by demo-monitor-trends.tape: its fixed plotting window needs
# ~4 min of points to fill, too long for this tour GIF.
#
# Showcases the live operator TUI exposed by `perf-sentinel query
# monitor` (since 0.8.8): a daemon-backed dashboard that polls
# /api/export/report, /api/energy, /api/status, /api/config and
# /api/incidents and cycles six tabs with Tab: Advisor (settings-advisor
# hints), Energy (the effective energy/carbon mix per service and per
# region), Trends (braille charts of the energy/carbon per window plus
# runtime gauges as a share of their caps), Scrapers (live backend
# health), Config (the read-only daemon parameters) and Incidents (the
# restarts and memory events the alerting posted, each with the findings
# frozen from its window, since 0.20.0, the one tab that takes an API
# key). This tape covers the nominal, well-dimensioned daemon: the Energy
# tab is rich and the Advisor tab reads "No hints". The undersized-daemon
# Advisor hint and the Scrapers still frame are captured separately by
# demo-monitor-advisor.tape and demo-monitor-scrapers.tape at shorter
# heights.
#
# The daemon runs with [daemon.incidents] and a [daemon] read_api_key.
# One Alertmanager envelope (an OOM kill of checkout-svc, started now) is
# posted with the write key after the findings are seeded, so the
# Incidents tab shows one incident with the findings of its window. The
# monitor itself opens with the read key only, the one that cannot POST.
# lookback_ms is shortened to 30 s so the window fits inside the daemon's
# uptime and the capture marker reads "complete": the 5 min default would
# reach back before the daemon started and read "partial", which is what
# the marker is for, but not what a nominal tour should show.
#
# Seeding mirrors demo-tui-ack.tape: the daemon's OTLP endpoints only
# accept application/x-protobuf, but [daemon] json_socket (a Unix domain
# socket of NDJSON SpanEvent arrays) takes the native fixture directly.
# A short trace_ttl_ms flushes the TraceWindow so green scoring runs
# before the TUI opens. On top of the one-shot seed,
# scripts/demo-seed-loop.py keeps pushing randomly-sized sub-batches so
# the Trends curves actually move: each batch closes a new scoring
# window with a different energy total, and the findings store fills up
# tick after tick. [green] is enabled with per-service regions for the
# per-region table, and [green.cloud] points at an unreachable
# Prometheus endpoint so the Scrapers tab shows a configured backend
# whose freshness age climbs (offline-safe, no external network
# destination). --refresh 1 accumulates one trend point per second.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-monitor.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-monitor.tape   # generates monitor/demo.gif + 3 *.png
#   python3 scripts/trim-bottom-png.py docs/img/monitor/energy.png docs/img/monitor/incidents.png
#
#   # Post-process the GIF to keep it well under ~1 MB. Keep fps=20 (do not
#   # decimate) so the typed command stays smooth letter-by-letter. The
#   # 1000-wide capture needs no downscale, so scale keeps the native width:
#   ffmpeg -y -i docs/img/monitor/demo.gif \
#     -vf "fps=20,scale=1000:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
#     -loop 0 /tmp/demo-monitor-opt.gif
#   mv /tmp/demo-monitor-opt.gif docs/img/monitor/demo.gif

Output docs/img/monitor/demo.gif

Set Shell "bash"
Set FontSize 13
# Narrower and shorter than the 1600x900 the inspect TUIs use: the
# monitor's tabs are far less dense (a few summary tables, not
# multi-panel browsers), so the default viewport frames the content
# against a wide empty margin on the right and below. 1000 cols fits the
# widest line, which is now the Energy waste row (`Database waste: ... model
# estimated ... within the report totals`, ~120 cols) rather than the
# By-service row; at 810 the provenance tag was cut off mid-word. 600 fits
# the fullest tab (Energy, ~26 lines) and gives the two Trends chart rows
# ~12 lines each.
Set Width 1000
Set Height 600
Set TypingSpeed 100ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
# RUST_LOG=error keeps the daemon's info logs out of the alternate screen.
Type `export RUST_LOG=error`
Enter
Type `mkdir -p /tmp/demo-monitor-dir && cd /tmp/demo-monitor-dir && rm -f perf-sentinel.sock`
Enter
# Nominal daemon: short trace_ttl_ms so detectors and green scoring flush
# within ~1 s; per-service regions for a multi-row per-region table; a
# [green.cloud] backend pointed at an unreachable loopback endpoint so the
# Scrapers tab shows a configured backend whose freshness age climbs.
# max_active_traces and max_retained_findings are sized so the seed loop
# moves the Trends headroom curves visibly while staying well under the
# 90% advisor threshold (the Advisor tab must read "No hints").
Type `printf '[daemon]\njson_socket = "/tmp/demo-monitor-dir/perf-sentinel.sock"\ntrace_ttl_ms = 400\nmax_active_traces = 60\nmax_retained_findings = 500\nread_api_key = "demo-read-key-0123456"\n\n[daemon.incidents]\nenabled = true\napi_key = "demo-incidents-key-0123"\nlookback_ms = 30000\n\n[green]\nenabled = true\ndefault_region = "eu-west-3"\n\n[green.service_regions]\ncheckout-svc = "us-east-1"\ngateway-svc = "eu-west-1"\ndashboard-svc = "ap-southeast-2"\nreports-svc = "us-west-1"\n\n[green.cloud]\nprometheus_endpoint = "http://127.0.0.1:19999/metrics"\nscrape_interval_secs = 2\n\n[green.cloud.services]\norder-svc = { idle_watts = 5.0, max_watts = 30.0 }\n' > .perf-sentinel.toml`
Enter
Type `perf-sentinel watch --config .perf-sentinel.toml --listen-port-http 14318 --listen-port-grpc 14319 > /tmp/demo-monitor-daemon.log 2>&1 &`
Enter
Type `sleep 2`
Enter
# One-shot seed for the Energy tables, then the background loop that
# keeps the Trends curves moving (random sub-batches, unique trace ids).
Type `python3 -c "import socket, json; e=json.load(open('$OLDPWD/tests/fixtures/demo.json')); s=socket.socket(socket.AF_UNIX); s.connect('/tmp/demo-monitor-dir/perf-sentinel.sock'); s.sendall((json.dumps(e)+chr(10)).encode()); s.shutdown(socket.SHUT_WR)"`
Enter
Type `python3 "$OLDPWD/scripts/demo-seed-loop.py" /tmp/demo-monitor-dir/perf-sentinel.sock "$OLDPWD/tests/fixtures/demo.json" 1.3 checkout-svc,dashboard-svc,notify-svc > /dev/null 2>&1 &`
Enter
# The incident: once the seeded windows have flushed (> trace_ttl_ms),
# post one Alertmanager envelope with the write key, an OOM kill of
# checkout-svc starting now, so the daemon freezes the findings the ring
# holds for that service. The read key file feeds the monitor below.
Type `sleep 2 && curl -s -o /dev/null -X POST http://127.0.0.1:14318/api/incidents -H 'X-API-Key: demo-incidents-key-0123' -H 'Content-Type: application/json' -d "{\"version\":\"4\",\"status\":\"firing\",\"alerts\":[{\"status\":\"firing\",\"labels\":{\"service\":\"checkout-svc\",\"perf_sentinel_kind\":\"oom_kill\"},\"annotations\":{\"summary\":\"container exceeded its memory limit\"},\"startsAt\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"endsAt\":\"0001-01-01T00:00:00Z\"}]}"`
Enter
Type `printf 'demo-read-key-0123456' > read-key && chmod 600 read-key`
Enter
# Wait > trace_ttl_ms so windows flush, and long enough for a few cloud
# scrape failures so the Scrapers age column is visibly non-zero. The VHS
# sleep must outlast the shell `sleep 6` so `clear` runs before Show,
# otherwise the hidden setup commands leak into the first frames.
Type `sleep 6 && clear`
Enter
Sleep 9000ms
Show

# Open the live monitor against the daemon. --daemon is explicit because
# the query subcommand does not read PERF_SENTINEL_DAEMON_URL. A 1 s
# refresh lands one trend point per second, so the Trends charts have a
# real curve by the time the tape reaches them. The key file holds the
# read key: it opens the Incidents tab and nothing else.
Type "perf-sentinel query --daemon http://127.0.0.1:14318 monitor --refresh 1 --api-key-file read-key"
Sleep 800ms
Enter
# Opens on the Advisor tab. A well-dimensioned daemon reports no hints.
Sleep 2500ms

# Tab -> Energy: the per-service and per-region mix.
Tab
Sleep 2500ms
Screenshot docs/img/monitor/energy.png
Sleep 1500ms

# Tab -> Trends: the energy/carbon and headroom charts. The poller fills
# the fixed window from empty, so the tour only shows the right edge
# building up; the filled-window still is captured by
# demo-monitor-trends.tape.
Tab
Sleep 6s

# Tab -> Scrapers: backend health (still frame owned by its own tape).
Tab
Sleep 3s

# Tab -> Config: the read-only daemon parameters (current, default, what
# each does). trace_ttl_ms is flagged "modified" against its default.
Tab
Sleep 2500ms
Screenshot docs/img/monitor/config.png
Sleep 1500ms

# Tab -> Incidents: the seeded OOM kill of checkout-svc with the findings
# of its window, read with the read-only key.
Tab
Sleep 2500ms
Screenshot docs/img/monitor/incidents.png
Sleep 1500ms

# Tab -> back to Advisor, closing the cycle.
Tab
Sleep 1500ms

Type "q"
Sleep 500ms

Hide
Type "kill %1 %2 2> /dev/null; wait 2> /dev/null"
Enter
Sleep 300ms
