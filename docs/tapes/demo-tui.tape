# The gtunnel TUI, filled with the synthetic data `gtunnel demo` provides.
# It touches no GCP project and no configuration file.
# `target/release` goes first on PATH, so the recording shows the binary this
# checkout builds (`cargo build --release`), never an older installed one.
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
# An empty configuration directory: any command gtunnel runs at start-up, such
# as reinstalling a launchd job that predates the binary, then finds no job of
# the machine's own to name in the recording or to touch
Type `export GT=$(mktemp -d) && export PATH="$PWD/target/release:$PATH" GTUNNEL_CONFIG_DIR="$GT" GTUNNEL_NO_NOTIFY=1`
Enter
Type "clear"
Enter
Show

Sleep 500ms
Type "gtunnel demo"
Sleep 700ms
Enter
Sleep 3s

# Walk the tunnel list; the detail, log and metric panes follow the selection.
# Rows: an Access tunnel, a jump through `bastion`, a destination group, a run an
# agent opened (its tag and detail pane show when it stops), an IAP process
# gtunnel did not start, a public URL with load-balancer traffic, and an
# ephemeral URL behind IAP sign-in. The header rule shows an available update.
# Each managed row has its own log, and the selected Access row gains a
# connection open and close every few seconds: the first and third rows rest
# long enough for those lines to arrive in the Logs pane
Sleep 9s
Down
Sleep 2500ms
Down
Sleep 7s
Down
Sleep 3500ms

# `S` stops the selected run. The demo has no runtime, so it lands the stop at
# once: the row reads stopped and its health in Metrics is the stopped circle
Type "S"
Sleep 2500ms
Down
Sleep 1500ms
Down
Sleep 2500ms
Down
Sleep 2000ms
Up 6
Sleep 1500ms

# `e` chooses between editing the target and ports, renaming, and moving
# the tunnel to another account
Type "e"
Sleep 2500ms
Escape
Sleep 1000ms

# `a` asks which kind of tunnel to add: Access or Public
Type "a"
Sleep 2500ms
Escape
Sleep 1000ms

# `:` is the accounts page: add, edit, select and remove accounts
Type ":"
Sleep 3s

# `a` adds one: the first step picks a gcloud configuration, with the active
# one already selected
Type "a"
Sleep 3s
Down
Sleep 1500ms
Escape
Sleep 1000ms

# `e` edits the selected account: a project picker, then the zone picker
Type "e"
Sleep 2500ms
Enter
Sleep 3s
Escape
Sleep 1000ms
Escape
Sleep 1000ms

# `;` cycles the accounts; `staging` has no relay, so it shows Access only
Type ";"
Sleep 2500ms
Type ";"
Sleep 1500ms

# `l` copies this tunnel's log to the clipboard
Type "l"
Sleep 2000ms

# `?` is the key map
Type "?"
Sleep 4s
Escape
Sleep 1500ms

Type "q"
Sleep 1500ms

Hide
Type `rm -rf "$GT"`
Enter
