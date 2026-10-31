# An ephemeral tunnel with a stop time: `run --ttl`, the list that shows it,
# Ctrl+C, and the empty list it leaves.
#
# The hidden preamble is demo-access.tape's: a throwaway account in a temporary
# directory and a stand-in `gcloud` on PATH, so the recording needs no GCP
# project. An Access `run` resolves the instance in the account's default zone
# without a Compute lookup, so the stand-in is enough. Every line gtunnel prints
# below is its own, unedited.
#
# The run goes to the background with `&` so `gtunnel list` can show it while it
# lives, and `sleep 2` lets its first lines print before the next prompt; `fg`
# brings it back for Ctrl+C. The `[1] <pid>` line is the shell's.
#
# `target/release` goes first on PATH, so the recording shows the binary this
# checkout builds (`cargo build --release`), never an older installed one.
#
#   vhs docs/tapes/demo-run.tape
#
Output docs/demo-run.gif

Require gtunnel

Set FontSize 16
Set Width 1500
Set Height 500
Set Padding 24
Set TypingSpeed 55ms

Hide
Type `export GT=$(mktemp -d) && mkdir -p "$GT/bin" "$GT/cfg"`
Enter
Type `printf '#!/bin/sh\ncase "$1" in auth) echo ya29.demo-token; exit 0 ;; esac\nfor a; do case "$a" in --local-host-port=*) echo "Listening on port [${a##*:}]." >&2 ;; esac; done\nexec sleep 3600\n' > "$GT/bin/gcloud" && chmod +x "$GT/bin/gcloud"`
Enter
Type `printf '#!/bin/sh\n/bin/ps "$@" | grep -v start-iap-tunnel || true\n' > "$GT/bin/ps" && chmod +x "$GT/bin/ps"`
Enter
Type `printf 'selected_account = "demo"\n[[accounts]]\nname = "demo"\ngcloud_configuration = "demo"\nproject = "example-dev-1a2b"\ndefault_zone = "europe-west1-b"\n' > "$GT/cfg/config.toml"`
Enter
Type `export PATH="$PWD/target/release:$GT/bin:$PATH" GTUNNEL_CONFIG_DIR="$GT/cfg" PS1="$ "`
Enter
# A fresh update cache, so the recording carries no "newer release" hint that
# would date it
Type `printf '{"latest_version":"%s","checked_at":%s}' "$(gtunnel --version | cut -d" " -f2)" "$(date +%s)" > "$GT/cfg/update-check.json"`
Enter
Type "clear"
Enter
Show

Sleep 800ms
Type "gtunnel run --ttl 30m db bastion:5432 & sleep 2"
Sleep 600ms
Enter
# the port line and the stop time follow
Sleep 4s

Type "gtunnel list"
Sleep 400ms
Enter
Sleep 3s

Type "fg"
Sleep 400ms
Enter
Sleep 1500ms
Ctrl+C
Sleep 2s

Type "gtunnel list"
Sleep 400ms
Enter
Sleep 3s

Hide
Type `rm -rf "$GT"`
Enter
