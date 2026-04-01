# VHS script — generates docs/img/demo.gif
# Install vhs: https://github.com/charmbracelet/vhs
# Prerequisites: cargo build --release

Output docs/img/demo.gif

Set Shell "bash"
Set FontSize 13
Set Width 1200
Set Height 900
Set TypingSpeed 40ms
Set Padding 15

Hide
Type `export PATH=$PWD/target/release:$PATH`
Enter
Type `mkdir -p /tmp/demo-dir`
Enter
Type `printf '[thresholds]\nio_waste_ratio_max = 0.30\n\n[green]\nregion = "eu-west-3"\n' > /tmp/demo-dir/.perf-sentinel.toml`
Enter
Type `cp tests/fixtures/mixed.json /tmp/demo-dir/traces.json`
Enter
Type `cd /tmp/demo-dir && clear`
Enter
Sleep 500ms
Show

Type "cat .perf-sentinel.toml"
Sleep 500ms
Enter
Sleep 3s

Type "perf-sentinel analyze --input traces.json"
Sleep 1s
Enter
Sleep 1s

# Hold the output for reading
Sleep 10s
