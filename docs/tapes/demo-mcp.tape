# An agent opens a tunnel for 30 minutes, the list shows it, the agent closes it.
#
# Every request goes to the real `gtunnel mcp` server over stdio. `agent`, from
# docs/tapes/agent.sh, only performs the JSON-RPC handshake and prints the
# answer; every line below is gtunnel's own, unedited.
#
# The hidden preamble sets up a throwaway account in a temporary
# directory and a stand-in `gcloud` on PATH, so the recording needs no GCP
# project. `target/release` goes first on PATH, so the recording shows the
# binary this checkout builds (`cargo build --release`).
#
#   vhs docs/tapes/demo-mcp.tape
#
Output docs/demo-mcp.gif

Require gtunnel
Require jq

Set FontSize 16
Set Width 1500
Set Height 500
Set Padding 24
Set TypingSpeed 45ms

Hide
Type `export GT=$(mktemp -d) && mkdir -p "$GT/bin" "$GT/cfg"`
Enter
Type `printf '#!/bin/sh\ncase "$1" in auth) echo ya29.demo-token; exit 0 ;; esac\nfor a; do case "$a" in --local-host-port=*) echo "Listening on port [${a##*:}]." >&2 ;; esac; done\nexec sleep 3600\n' > "$GT/bin/gcloud" && chmod +x "$GT/bin/gcloud"`
Enter
Type `printf '#!/bin/sh\n/bin/ps "$@" | grep -v start-iap-tunnel || true\n' > "$GT/bin/ps" && chmod +x "$GT/bin/ps"`
Enter
Type `printf 'selected_account = "demo"\n[[accounts]]\nname = "demo"\ngcloud_configuration = "demo"\nproject = "example-dev-1a2b"\ndefault_zone = "europe-west1-b"\n' > "$GT/cfg/config.toml"`
Enter
Type `export PATH="$PWD/target/release:$GT/bin:$PATH" GTUNNEL_CONFIG_DIR="$GT/cfg" GTUNNEL_NO_NOTIFY=1 PS1="$ " && . docs/tapes/agent.sh`
Enter
# A fresh update cache, so the recording carries no "newer release" hint that
# would date it
Type `printf '{"latest_version":"%s","checked_at":%s}' "$(gtunnel --version | cut -d" " -f2)" "$(date +%s)" > "$GT/cfg/update-check.json"`
Enter
Type "clear"
Enter
Show

Sleep 800ms
Type "agent tools"
Sleep 400ms
Enter
Sleep 2500ms

Type `agent gtunnel_run_tunnel '{"name": "db", "target": "bastion:5432", "ttl_minutes": 30}'`
Sleep 500ms
Enter
# the tool waits for the run's record before it answers
Sleep 4s

Type "gtunnel list"
Sleep 400ms
Enter
Sleep 3s

Type `agent gtunnel_stop_tunnel '{"name": "db"}'`
Sleep 400ms
Enter
Sleep 2500ms

Type "gtunnel list"
Sleep 400ms
Enter
Sleep 2500ms

Hide
Type `rm -rf "$GT"`
Enter
