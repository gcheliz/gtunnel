Title: fix(supervise): stop tunnels retrying and starting in lockstep after a wake, login or outage

Stacks on #370 (PR #406). #378, #379 and #374 are already on main.

## What changed

| Part | Change |
|---|---|
| Backoff | `src/daemon/backoff.rs`: one function draws a wait from zero up to the doubling's ceiling (full jitter). The supervisor's restart and network waits and the breaker's schedule all call it; no second doubling remains |
| First start | A supervisor that launchd or systemd started on its own waits a random 0-3 s before its first `gcloud`. `daemon::start` leaves `run/<label>.asked`, which the next supervisor deletes and reads as "someone asked", so `gtunnel start` and the watchdog skip the wait |
| Watchdog | Pauses a random 0-2 s before each restart in a pass and before each tunnel it resumes after a renewed login (`resume.rs`; on-demand `start --auth-expired` and the MCP tool do not pause). The pause comes before the stop, so the tunnel is not down for it, and a person who acts during it is still read |
| TUI | One `GcpClient` per account for the session (`ClientPool`, on top of #374's token cache and expiry margin, so a token near expiry is still refetched), so a load-balancer round over several public tunnels fetches one token. An edited account gets a new client |
| Tests | A debug-only `GTUNNEL_TEST_JITTER` pins the draw for the real-binary supervisor tests, as `GTUNNEL_TEST_GCP_ENDPOINT` does for the endpoint |

## Why

Deterministic backoff keeps tunnels that failed together failing together. Each `gcloud` start is a Python process that reads the credential store, so N of them in one second is the expensive case for the laptop and for Google.

## Impact

The ceiling of every wait is the old schedule, so a lone tunnel's worst case is unchanged; its average is half. A tunnel that auto-starts at login may come up up to 3 s later. Docs: `architecture.md`, `troubleshooting.md`, `configuration.md`, `decisions.md`.

## Not covered

- The manual acceptance item (five tunnels after a real wake, starts spread over more than one second) needs a laptop and a wake.
- #371 (PR #408) is still open: its fixed 15 s busy-listener retry is not in this base and is untouched. It is a retry of the same kind and should draw from `backoff` once #408 lands.

Refs #380
