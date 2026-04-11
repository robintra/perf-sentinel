# VHS script, generates docs/img/pg-stat/demo.gif and docs/img/pg-stat/hotspots.png
#
# Showcases the `perf-sentinel pg-stat` mode, which reads a PostgreSQL
# `pg_stat_statements` export (CSV or JSON) and ranks the slowest /
# most-called / highest-mean-time queries. Useful for cross-referencing
# DB hotspots with traces: a query that dominates `pg_stat_statements`
# without appearing in your traces is a sign of instrumentation gaps.
#
# The demo uses `--top-n 3` to keep the output readable (~47 lines, fits
# in a single Height 1000 viewport). The default `--top-n 10` produces
# ~131 lines across three rankings (by total_exec_time, by calls, by
# mean_exec_time) which would require scrolling.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-pg-stat.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-pg-stat.tape   # generates pg-stat/demo.gif + pg-stat/hotspots.png
#
#   # Optional: post-process the GIF if it grows past ~1 MB (raw output
#   # is typically well under that thanks to Framerate 20 + PlaybackSpeed 2.0).
#   ffmpeg -y -i docs/img/pg-stat/demo.gif \
#     -vf "fps=10,scale=800:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
#     -loop 0 /tmp/demo-pg-stat-opt.gif
#   mv /tmp/demo-pg-stat-opt.gif docs/img/pg-stat/demo.gif

Output docs/img/pg-stat/demo.gif

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
Type `mkdir -p /tmp/demo-pg-stat-dir`
Enter
Type `cp tests/fixtures/pg_stat_statements.csv /tmp/demo-pg-stat-dir/pg_stat.csv`
Enter
Type `cd /tmp/demo-pg-stat-dir && clear`
Enter
Sleep 500ms
Show

Type "perf-sentinel pg-stat --input pg_stat.csv --top-n 3"
Sleep 1s
Enter
Sleep 3s
Screenshot docs/img/pg-stat/hotspots.png
Sleep 5s
