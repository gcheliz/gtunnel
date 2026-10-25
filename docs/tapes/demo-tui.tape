# The gtunnel TUI, filled with the synthetic data `gtunnel demo` provides.
# It touches no GCP project and no configuration file.
#
#   vhs docs/tapes/demo-tui.tape
#
Output docs/demo-tui.gif

Require gtunnel

Set FontSize 16
Set Width 1500
Set Height 880
Set Padding 24
Set TypingSpeed 70ms

Hide
Type "clear"
Enter
Show

Sleep 500ms
Type "gtunnel demo"
Sleep 700ms
Enter
Sleep 3s

# Walk the tunnel list; the detail, log and metric panes follow the selection
Down
Sleep 1500ms
Down
Sleep 1500ms
Down
Sleep 2s
Up
Sleep 1500ms

# `;` cycles the accounts; `staging` has no relay, so it shows Access only
Type ";"
Sleep 2s
Type ";"
Sleep 2s

# `l` copies this tunnel's log to the clipboard
Type "l"
Sleep 2500ms

# `?` is the key map
Type "?"
Sleep 4s
Escape
Sleep 1500ms

Type "q"
Sleep 1500ms
