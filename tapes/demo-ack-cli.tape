# VHS script, generates docs/img/ack/cli.gif and three still frames
# (cli-create.png, cli-list.png, cli-revoke.png).
#
# Showcases the `perf-sentinel ack` subcommand (since 0.5.22): three
# subactions `create`, `list`, `revoke` against a running daemon. The
# tape spawns a daemon in the background on a non-default port to
# avoid colliding with any locally-running instance, then types the
# three create / list / revoke commands one after the other in a
# single shell session so the viewer sees a continuous workflow
# (no `clear` between commands). Each Screenshot captures the full
# terminal at that moment, which `scripts/trim-bottom-png.py` then
# crops to the last non-empty row.
#
# The ack store is configured with an explicit `storage_path` because
# the default location may collide with a stale acks.jsonl on the
# generation machine. Using `./acks.jsonl` keeps the tape hermetic.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-ack-cli.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-ack-cli.tape
#   python3 scripts/trim-bottom-png.py docs/img/ack/cli-*.png

Output docs/img/ack/cli.gif

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
Type `mkdir -p /tmp/demo-ack-dir && cd /tmp/demo-ack-dir`
Enter
Type `printf '[daemon.ack]\nenabled = true\nstorage_path = "./acks.jsonl"\n' > .perf-sentinel.toml`
Enter
Type `rm -f acks.jsonl`
Enter
# Background daemon on dedicated ports, log diverted so the tape stays clean.
Type `perf-sentinel watch --config .perf-sentinel.toml --listen-port-http 14318 --listen-port-grpc 14319 > /tmp/demo-ack-daemon.log 2>&1 &`
Enter
Type `sleep 2 && export PERF_SENTINEL_DAEMON_URL=http://127.0.0.1:14318 && clear`
Enter
Sleep 2s
Show

# 1. Create the first ack and screenshot the standalone success footer.
Type "perf-sentinel ack create --signature n_plus_one_sql:order-svc:_api_v1_orders:0123456789abcdef0123456789abcdef --reason 'Known intentional N+1 in checkout, JIRA-1234' --expires 7d --by alice@example.com"
Sleep 600ms
Enter
Sleep 2500ms
Screenshot docs/img/ack/cli-create.png
Sleep 1500ms

# 2. Two more creates back-to-back, no clear: the terminal accumulates.
Type "perf-sentinel ack create --signature slow_sql:payment-svc:_internal_balance:fedcba9876543210fedcba9876543210 --reason 'Slow batch SQL pending optimisation' --expires 30d --by bob@example.com"
Sleep 600ms
Enter
Sleep 1500ms

Type "perf-sentinel ack create --signature redundant_http:auth-svc:_api_v1_validate:1111222233334444aaaabbbbccccdddd --reason 'Redundant token validation, fix in v0.7.0' --by ops-bot"
Sleep 600ms
Enter
Sleep 1500ms

# 3. List the three active acks. The screenshot captures the cumulative
# session up to and including the table.
Type "perf-sentinel ack list"
Sleep 600ms
Enter
Sleep 2500ms
Screenshot docs/img/ack/cli-list.png
Sleep 2500ms

# 4. Revoke the slow_sql ack, still in the same session.
Type "perf-sentinel ack revoke --signature slow_sql:payment-svc:_internal_balance:fedcba9876543210fedcba9876543210"
Sleep 600ms
Enter
Sleep 2500ms
Screenshot docs/img/ack/cli-revoke.png
Sleep 2s

Hide
Type "kill %1 2> /dev/null; wait 2> /dev/null"
Enter
Sleep 300ms
