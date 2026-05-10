# VHS script, generates docs/img/ack/tui-modal.png + tui-confirm.png
# plus a companion GIF docs/img/ack/tui.gif.
#
# Showcases the live TUI ack flow exposed by `perf-sentinel query
# inspect` (since 0.5.24): the user navigates to a finding, presses
# `a` to open the ack modal, fills reason + expires + by, submits,
# and the finding moves out of the active list. The companion `u`
# key (revoke) is not exercised here because one screenshot per
# action keeps the tape compact, but the modal renders the same way.
#
# Seeding the daemon with findings is the load-bearing step: the
# daemon's OTLP endpoints only accept `application/x-protobuf`, but
# `[daemon] json_socket` (Unix domain socket, NDJSON of SpanEvent
# arrays, owner-only 0600 permissions) accepts our native fixture
# format directly. A short `trace_ttl_ms = 500` config knob forces
# the TraceWindow to flush within 1 s so the detectors fire before
# the tape opens the TUI.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-tui-ack.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-tui-ack.tape
#   python3 scripts/trim-bottom-png.py docs/img/ack/tui-*.png

Output docs/img/ack/tui.gif

Set Shell "bash"
Set FontSize 13
Set Width 1600
Set Height 1000
Set TypingSpeed 40ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
# Override $USER so the ack modal's `By` field pre-fills with a
# neutral identifier instead of the operator's local username (which
# would otherwise leak into a public asset).
Type `export USER=demo-user`
Enter
Type `mkdir -p /tmp/demo-tui-ack-dir && cd /tmp/demo-tui-ack-dir && rm -f acks.jsonl perf-sentinel.sock`
Enter
# Daemon config: short trace TTL flushes the window within 1 s so the
# detectors emit findings before the tape opens the TUI. The JSON
# socket lives next to acks.jsonl in the scratch dir to avoid the
# default /tmp path racing with any other daemon.
Type `printf '[daemon]\njson_socket = "/tmp/demo-tui-ack-dir/perf-sentinel.sock"\ntrace_ttl_ms = 500\n\n[daemon.ack]\nenabled = true\nstorage_path = "./acks.jsonl"\n' > .perf-sentinel.toml`
Enter
Type `perf-sentinel watch --config .perf-sentinel.toml --listen-port-http 14318 --listen-port-grpc 14319 > /tmp/demo-tui-ack-daemon.log 2>&1 &`
Enter
Type `sleep 2`
Enter
# Push the demo fixture as one NDJSON line through the Unix socket.
# Python is preferred over nc -U for portable shutdown semantics.
Type `python3 -c "import socket, json; e=json.load(open('$OLDPWD/tests/fixtures/demo.json')); s=socket.socket(socket.AF_UNIX); s.connect('/tmp/demo-tui-ack-dir/perf-sentinel.sock'); s.sendall((json.dumps(e)+chr(10)).encode()); s.shutdown(socket.SHUT_WR)"`
Enter
# Wait > trace_ttl_ms so the detectors flush their windows.
Type `sleep 2 && clear`
Enter
Sleep 2s
Show

# 1. Open the live TUI against the daemon. The Findings panel
# auto-populates from the daemon snapshot (10 findings from the demo
# fixture, sorted by severity). `--daemon` is explicit because the
# `query` subcommand does not pick up PERF_SENTINEL_DAEMON_URL (only
# the `ack` subcommand does, see ack.rs).
Type "perf-sentinel query --daemon http://127.0.0.1:14318 inspect"
Sleep 600ms
Enter
Sleep 3s

# Move down a couple of rows to land on a representative finding
# instead of the very first row, then press `a` to open the ack
# modal. The modal pre-fills the signature read-only and waits on
# Reason as the required field.
Down
Sleep 500ms
Down
Sleep 500ms
Type "a"
Sleep 1500ms
Screenshot docs/img/ack/tui-modal.png
Sleep 2s

# Fill the form: reason -> Tab -> expires shortcut -> Tab to skip
# the By field (pre-filled from $USER) -> Tab to land on the
# Acknowledge button. A longer pause on the filled modal lets the
# GIF viewer read the form before the Enter submits it.
Type "Known intentional batch in checkout, JIRA-1234"
Sleep 800ms
Tab
Sleep 400ms
Type "7d"
Sleep 800ms
Tab
Sleep 400ms
Tab
Sleep 400ms
Screenshot docs/img/ack/tui-modal-filled.png
Sleep 3500ms
Enter
Sleep 2500ms
Screenshot docs/img/ack/tui-confirm.png
Sleep 2s

Type "q"
Sleep 500ms

Hide
Type "kill %1 2> /dev/null; wait 2> /dev/null"
Enter
Sleep 300ms
