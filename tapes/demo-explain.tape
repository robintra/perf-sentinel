# VHS script, generates docs/img/explain/demo.gif and two still frames
# (docs/img/explain/tree.png, docs/img/explain/trace-level.png).
# Companion to tapes/demo.tape. Showcases the two rendering paths of the
# `perf-sentinel explain` mode:
#
#   1. span-anchored annotations — findings whose `pattern.template`
#      matches a span template are rendered inline next to the span.
#      Demonstrated via the fanout trace (trace-demo-fanout), where
#      `← excessive fanout warning (×21)` lands cleanly on the parent
#      span and the 21 children are rendered in a compact tree below.
#
#   2. trace-level findings header — findings whose `pattern.template`
#      is a service name or an entry endpoint (chatty service, pool
#      saturation, serialized calls) cannot anchor on a single span
#      and are surfaced in a dedicated `⚠ Trace-level findings:`
#      header above the span tree. Demonstrated via the chatty trace
#      (trace-demo-chatty).
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-explain.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-explain.tape   # generates explain/demo.gif + tree.png + trace-level.png
#
#   # Optional: post-process the GIF if it grows past ~1 MB (current raw
#   # output is ~115 KB thanks to Framerate 20 + PlaybackSpeed 2.0 + a
#   # small Height 650, so no optimization is usually needed).
#   ffmpeg -y -i docs/img/explain/demo.gif \
#     -vf "fps=10,scale=800:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
#     -loop 0 /tmp/demo-explain-opt.gif
#   mv /tmp/demo-explain-opt.gif docs/img/explain/demo.gif

Output docs/img/explain/demo.gif

Set Shell "bash"
Set FontSize 13
Set Width 1200
Set Height 650
Set TypingSpeed 40ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `mkdir -p /tmp/demo-explain-dir`
Enter
Type `cp tests/fixtures/demo.json /tmp/demo-explain-dir/traces.json`
Enter
Type `cd /tmp/demo-explain-dir && clear`
Enter
Sleep 500ms
Show

Type "perf-sentinel explain --input traces.json --trace-id trace-demo-fanout"
Sleep 1s
Enter
Sleep 3s
Screenshot docs/img/explain/tree.png
Sleep 4s

Type "clear"
Enter
Sleep 2s

Type "perf-sentinel explain --input traces.json --trace-id trace-demo-chatty"
Sleep 1s
Enter
Sleep 3s
Screenshot docs/img/explain/trace-level.png
Sleep 4s
