# VHS script, generates docs/img/analyze/config.png
# Companion to tapes/demo.tape. This one captures a compact PNG of the
# config file content only. Kept separate because the main demo.tape
# uses Height 900 to fit the full analyze report, which would leave a
# large empty black area in the config-only screenshot.
#
# Run from the repo root: `vhs tapes/demo-config.tape`.
#
# The `Output` below writes an unused .gif byproduct under `tapes/` (VHS
# requires an `Output` directive per tape). It is gitignored.
#
# Install vhs: https://github.com/charmbracelet/vhs

Output tapes/demo-config-unused.gif

Set Shell "bash"
Set FontSize 13
Set Width 1200
Set Height 250
Set TypingSpeed 40ms
Set Padding 15

Hide
Type `mkdir -p /tmp/demo-config-dir`
Enter
Type `printf '[thresholds]\nio_waste_ratio_max = 0.10\n\n[green]\ndefault_region = "eu-west-3"\n' > /tmp/demo-config-dir/.perf-sentinel.toml`
Enter
Type `cd /tmp/demo-config-dir && clear`
Enter
Sleep 500ms
Show

Type "cat .perf-sentinel.toml"
Sleep 500ms
Enter
Sleep 2s
Screenshot docs/img/analyze/config.png
Sleep 500ms
