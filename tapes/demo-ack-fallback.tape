# VHS script, generates docs/img/ack/fallback-default.png and
# fallback-show.png plus a small companion GIF docs/img/ack/fallback.gif.
#
# Showcases the TOML-based CI ack flow (`.perf-sentinel-acknowledgments.toml`):
# two findings from the demo fixture are acked, the default `analyze`
# output reports them as suppressed (and the quality gate flips to
# PASSED because the acked findings were the criticals), then
# `--show-acknowledged` surfaces the per-ack metadata footer
# (signature, by, at, reason). The two analyze runs are typed
# back-to-back without `clear`, the terminal scroll naturally pushes
# the first run off the top as the second one streams in.
#
# Kept separate from `tapes/demo.tape` so the main analyze GIF and
# its four still frames stay focused on the unfiltered report.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-ack-fallback.tape`.

Output docs/img/ack/fallback.gif

Set Shell "bash"
Set FontSize 13
Set Width 1200
Set Height 1000
Set TypingSpeed 40ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `mkdir -p /tmp/demo-ack-fallback-dir && cd /tmp/demo-ack-fallback-dir`
Enter
Type `cp $OLDPWD/tests/fixtures/demo.json traces.json`
Enter
# Two acks pinned to real signatures from the demo fixture: the
# critical N+1 SQL and the warning N+1 HTTP, both on POST /api/orders/42/submit.
Type `cat > .perf-sentinel-acknowledgments.toml <<'EOF'`
Enter
Type `[[acknowledged]]`
Enter
Type `signature = "n_plus_one_sql:order-svc:POST__api_orders_42_submit:c69d2f3ae7f5c6cdb2c6762367852ec7"`
Enter
Type `acknowledged_by = "alice@example.com"`
Enter
Type `acknowledged_at = "2026-05-02"`
Enter
Type `reason = "Known intentional batch in checkout, JIRA-1234"`
Enter
Type ``
Enter
Type `[[acknowledged]]`
Enter
Type `signature = "n_plus_one_http:order-svc:POST__api_orders_42_submit:30ce6c042411888a2bd69374d0dac255"`
Enter
Type `acknowledged_by = "bob@example.com"`
Enter
Type `acknowledged_at = "2026-05-04"`
Enter
Type `reason = "Pending move to gRPC, see ADR-0042"`
Enter
Type `expires_at = "2026-12-31"`
Enter
Type `EOF`
Enter
Type `clear`
Enter
Sleep 500ms
Show

# 1. Default analyze: the long output streams in, terminal scroll
# naturally lands "Quality gate: PASSED" + the suppressed-acks hint
# at the bottom of the viewport.
Type "perf-sentinel analyze --input traces.json"
Sleep 600ms
Enter
Sleep 3s
Screenshot docs/img/ack/fallback-default.png
Sleep 2500ms

# 2. Same run with --show-acknowledged, typed in the same session.
# As the new output streams in, the previous run scrolls off the top
# and the per-ack metadata footer (signature, by, at, reason) lands
# at the bottom of the viewport.
Type "perf-sentinel analyze --input traces.json --show-acknowledged"
Sleep 600ms
Enter
Sleep 3s
Screenshot docs/img/ack/fallback-show.png
Sleep 2500ms
