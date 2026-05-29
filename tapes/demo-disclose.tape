# VHS script, generates docs/img/disclose/demo.gif and three still frames
# (preview.png, quarter.png, official.png).
#
# Showcases the read-only `disclose --tui` preview: it re-reads the cold
# NDJSON archive (the same path as the canonical command), aggregates over
# a period, and renders a settings header, an aggregated summary, and the
# equivalent `disclose` command in the footer. It never hashes or writes a
# report.
#
# The hidden setup synthesises a small Q1 2026 archive from the demo
# fixture: `analyze --format json` produces one Report, and jq wraps it
# under six weekly `{ts, report}` envelopes. The preview opens on the
# archive's last month (March), `g` widens the granularity to the whole
# quarter, and `i` flips the intent to official to surface the validator
# verdict (the demo data predates per-service attribution, so coverage is
# low and the official validator reports why, which is exactly what the
# preview is for).
#
# Frames:
#   - preview.png : default Month view (internal intent, public/G2)
#   - quarter.png : after `g`, the whole quarter (six windows)
#   - official.png: after `i`, the official-validator verdict
#
# RUST_LOG=error keeps the alternate screen clean: each settings change
# re-aggregates the cold archive, and the demo data trips a fallback
# warning that would otherwise leak into the TUI render.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
# Requires jq for the archive synthesis.
#
# Run from the repo root: `vhs tapes/demo-disclose.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-disclose.tape   # generates disclose/demo.gif + 3 *.png
#
#   # Post-process the GIF to keep it well under ~1 MB. Keep fps=20 (do not
#   # decimate) so the typed command stays smooth letter-by-letter:
#   ffmpeg -y -i docs/img/disclose/demo.gif \
#     -vf "fps=20,scale=1000:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
#     -loop 0 /tmp/demo-disclose-opt.gif
#   mv /tmp/demo-disclose-opt.gif docs/img/disclose/demo.gif

Output docs/img/disclose/demo.gif

Set Shell "bash"
Set FontSize 13
# Sized to the preview content (narrow, short). The bordered summary box
# stretches to fill the height, so an oversized terminal would show large
# empty margins. Width keeps the footer command to two wrapped lines.
Set Width 1100
Set Height 560
# 100ms typing keeps the launch command at roughly one character per output
# frame (TypingSpeed / PlaybackSpeed = 50ms = one 20fps frame), so it reads
# as natural letter-by-letter typing rather than blocks. The post-process
# below must NOT decimate below this framerate or the typing turns blocky.
Set TypingSpeed 100ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `export RUST_LOG=error`
Enter
Type `rm -rf /tmp/demo-disclose-dir && mkdir -p /tmp/demo-disclose-dir`
Enter
Type `cp tests/fixtures/demo.json /tmp/demo-disclose-dir/demo.json`
Enter
Type `cp docs/examples/perf-sentinel-org.toml /tmp/demo-disclose-dir/org.toml`
Enter
Type `cd /tmp/demo-disclose-dir`
Enter
Type `perf-sentinel analyze --input demo.json --format json > r.json`
Enter
Type `for d in 2026-01-08 2026-01-22 2026-02-05 2026-02-19 2026-03-05 2026-03-19; do jq -c --arg ts "${d}T12:00:00Z" '{ts:$ts,report:.}' r.json; done > archive.ndjson`
Enter
Type `clear`
Enter
Sleep 500ms
Show

Sleep 1s
Type "perf-sentinel disclose --tui --input archive.ndjson --org-config org.toml"
Sleep 1s
Enter
Sleep 3s
Screenshot docs/img/disclose/preview.png
Sleep 2500ms

# Calendar stepper on populated months: step back to February, then return
# to March. Both carry windows, so the summary stays non-empty.
Left
Sleep 2500ms
Right
Sleep 2500ms

# `g` widens the granularity Month -> Quarter, covering the whole archive.
Type "g"
Sleep 2500ms
Screenshot docs/img/disclose/quarter.png
Sleep 2500ms

# `i` flips the intent to official, running the official validator.
Type "i"
Sleep 2500ms
Screenshot docs/img/disclose/official.png
# Hold the final validator verdict on screen before quitting.
Sleep 6s

Type "q"
