# VHS script, generates docs/img/calibrate/demo.gif and three still frames
# (docs/img/calibrate/csv.png, run.png, output.png).
#
# Showcases the `perf-sentinel calibrate` mode, which correlates a trace
# file with measured energy readings (from Scaphandre RAPL exports or
# cloud monitoring) and produces calibration factors written to a TOML
# file that can later be loaded via `[green] calibration_file`.
#
# The demo walks through the three steps of a realistic calibration
# session: inspect the energy CSV, run `perf-sentinel calibrate`, then
# inspect the generated calibration TOML.
#
# Install vhs: https://github.com/charmbracelet/vhs
# Install ffmpeg: https://ffmpeg.org/download.html
#
# Run from the repo root: `vhs tapes/demo-calibrate.tape`.
# Full regeneration workflow (run from the repo root):
#
#   cargo build --release
#   vhs tapes/demo-calibrate.tape   # generates calibrate/demo.gif + 3 *.png
#
#   # Post-process the GIF if it grows past ~1 MB.
#   ffmpeg -y -i docs/img/calibrate/demo.gif \
#     -vf "fps=10,scale=800:-1:flags=lanczos,split[s0][s1];[s0]palettegen=max_colors=64[p];[s1][p]paletteuse=dither=bayer:bayer_scale=5" \
#     -loop 0 /tmp/demo-calibrate-opt.gif
#   mv /tmp/demo-calibrate-opt.gif docs/img/calibrate/demo.gif

Output docs/img/calibrate/demo.gif

Set Shell "bash"
Set FontSize 13
Set Width 1200
Set Height 900
Set TypingSpeed 40ms
Set Padding 15
Set Framerate 20
Set PlaybackSpeed 2.0

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `mkdir -p /tmp/demo-calibrate-dir`
Enter
Type `cp tests/fixtures/demo.json /tmp/demo-calibrate-dir/traces.json`
Enter
Type `cp tests/fixtures/demo-energy.csv /tmp/demo-calibrate-dir/energy.csv`
Enter
Type `cd /tmp/demo-calibrate-dir && clear`
Enter
Sleep 500ms
Show

Type "cat energy.csv"
Sleep 500ms
Enter
Sleep 4s
Screenshot docs/img/calibrate/csv.png
Sleep 6s

Type "perf-sentinel calibrate --traces traces.json --measured-energy energy.csv"
Sleep 500ms
Enter
Sleep 4s
Screenshot docs/img/calibrate/run.png
Sleep 6s

Type "cat .perf-sentinel-calibration.toml"
Sleep 500ms
Enter
Sleep 4s
Screenshot docs/img/calibrate/output.png
Sleep 6s
