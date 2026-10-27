# An ephemeral tunnel from `run` to Ctrl+C, and the empty list it leaves.
#
# The hidden preamble is demo-access.tape's: a throwaway account in a temporary
# directory and a stand-in `gcloud` on PATH, so the recording needs no GCP
# project. An Access `run` resolves the instance in the account's default zone
# without a Compute lookup, so the stand-in is enough. Every line gtunnel prints
# below is its own, unedited.
#
# `gtunnel list` names declared tunnels only, so it is shown after Ctrl+C, to
# prove the run left nothing behind, not while the run holds the terminal.
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
Type `printf '#!/bin/sh\ncase "$1" in auth) echo ya29.demo-token ;; *) exec sleep 3600 ;; esac\n' > "$GT/bin/gcloud" && chmod +x "$GT/bin/gcloud"`
Enter
Type `printf '#!/bin/sh\n/bin/ps "$@" | grep -v start-iap-tunnel || true\n' > "$GT/bin/ps" && chmod +x "$GT/bin/ps"`
Enter
Type `printf 'selected_account = "demo"\n[[accounts]]\nname = "demo"\ngcloud_configuration = "demo"\nproject = "example-dev-1a2b"\ndefault_zone = "europe-west1-b"\n' > "$GT/cfg/config.toml"`
Enter
Type `export PATH="$PWD/target/release:$GT/bin:$PATH" GTUNNEL_CONFIG_DIR="$GT/cfg" PS1="$ "`
Enter
Type "clear"
Enter
Show

Sleep 800ms
Type "gtunnel run db my-vm:5432"
Sleep 600ms
Enter
# `run` holds the terminal; the port line and `Ctrl+C to stop` follow
Sleep 4s

Ctrl+C
Sleep 2s

Type "gtunnel list"
Sleep 400ms
Enter
Sleep 3s

Hide
Type `rm -rf "$GT"`
Enter
