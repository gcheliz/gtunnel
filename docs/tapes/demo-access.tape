# The life of an Access tunnel: add, start, stop, delete, and the error a
# mistyped name gets.
#
# The hidden preamble builds a throwaway account in a temporary directory and
# puts a stand-in `gcloud` on PATH, so the recording needs no GCP project. Every
# line gtunnel prints below is its own, unedited. The target is an IAP
# destination group, which is the one Access target gtunnel does not look up
# through the Compute API.
#
# The preamble also wraps `ps` to drop foreign `start-iap-tunnel` processes, so
# the listing shows this recording's tunnel rather than whatever else happens to
# be tunnelling on the machine doing the recording.
#
# The stand-in prints the `Listening on port [N].` line the real gcloud prints,
# which is what `start` waits for before it reports the tunnel running.
#
# `target/release` goes first on PATH, so the recording shows the binary this
# checkout builds (`cargo build --release`), never an older installed one.
#
#   vhs docs/tapes/demo-access.tape
#
Output docs/demo-access.gif

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
Type `export PATH="$PWD/target/release:$GT/bin:$PATH" GTUNNEL_CONFIG_DIR="$GT/cfg" GTUNNEL_NO_NOTIFY=1 PS1="$ "`
Enter
# A fresh update cache, so the recording carries no "newer release" hint that
# would date it
Type `printf '{"latest_version":"%s","checked_at":%s}' "$(gtunnel --version | cut -d" " -f2)" "$(date +%s)" > "$GT/cfg/update-check.json"`
Enter
Type "clear"
Enter
Show

Sleep 800ms
Type "gtunnel add db 10.0.0.5:5432 --dest-group prod-hosts --region europe-west1"
Sleep 600ms
Enter
Sleep 2s

Type "gtunnel list"
Sleep 400ms
Enter
Sleep 2500ms

Type "gtunnel start dv"
Sleep 400ms
Enter
Sleep 2500ms

Type "gtunnel start db"
Sleep 400ms
Enter
# `start` holds the terminal until the service reports running; typing the next
# command before it returns echoes a stray character into its output
Sleep 6s

Type "gtunnel list"
Sleep 400ms
Enter
Sleep 3s

Type "gtunnel stop db"
Sleep 400ms
Enter
Sleep 2500ms

Type "gtunnel delete db"
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
