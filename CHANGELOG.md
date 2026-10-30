# Changelog

Notable changes per release. Dates are the release date; an unreleased section is
what is on `main` and not yet tagged.

## [0.2.68](https://github.com/gcheliz/gtunnel/compare/v0.2.67...v0.2.68) - 2026-10-30

### Fixed

- *(state)* keep state files intact across gtunnel versions ([#399](https://github.com/gcheliz/gtunnel/pull/399))

## [0.2.67](https://github.com/gcheliz/gtunnel/compare/v0.2.66...v0.2.67) - 2026-10-30

### Fixed

- *(daemon)* give services the gcloud, proxy and CA environment of the shell ([#396](https://github.com/gcheliz/gtunnel/pull/396))

## [0.2.66](https://github.com/gcheliz/gtunnel/compare/v0.2.65...v0.2.66) - 2026-10-30

### Added

- *(update)* smoke-test the new binary, keep the old one and add --rollback ([#395](https://github.com/gcheliz/gtunnel/pull/395))

## [0.2.65](https://github.com/gcheliz/gtunnel/compare/v0.2.64...v0.2.65) - 2026-10-30

### Added

- *(relay)* update existing relay resources in place on relay up ([#394](https://github.com/gcheliz/gtunnel/pull/394))

## [0.2.64](https://github.com/gcheliz/gtunnel/compare/v0.2.63...v0.2.64) - 2026-10-30

### Fixed

- *(stop)* stop a tunnel without asking for a login ([#392](https://github.com/gcheliz/gtunnel/pull/392))

## [0.2.63](https://github.com/gcheliz/gtunnel/compare/v0.2.62...v0.2.63) - 2026-10-30

### Documentation

- *(readme)* credit ytunnel as the inspiration ([#367](https://github.com/gcheliz/gtunnel/pull/367))
- *(readme)* credit ytunnel and lay out the license ([#366](https://github.com/gcheliz/gtunnel/pull/366))

### Fixed

- *(doctor)* say the firewall rules could not be read instead of reporting one missing ([#393](https://github.com/gcheliz/gtunnel/pull/393))

## [0.2.62](https://github.com/gcheliz/gtunnel/compare/v0.2.61...v0.2.62) - 2026-10-30

### Fixed

- *(tui)* ask the terminal for wheel reports only, not every mouse move ([#363](https://github.com/gcheliz/gtunnel/pull/363))

## [0.2.61](https://github.com/gcheliz/gtunnel/compare/v0.2.60...v0.2.61) - 2026-10-30

### Fixed

- *(tui)* draw the stacked summary in the list's bottom border ([#361](https://github.com/gcheliz/gtunnel/pull/361))

## [0.2.60](https://github.com/gcheliz/gtunnel/compare/v0.2.59...v0.2.60) - 2026-10-30

### Fixed

- *(tui)* status messages that are not errors are never drawn ([#360](https://github.com/gcheliz/gtunnel/pull/360))

## [0.2.59](https://github.com/gcheliz/gtunnel/compare/v0.2.58...v0.2.59) - 2026-10-30

### Added

- *(notify)* desktop notifications are off until turned on ([#357](https://github.com/gcheliz/gtunnel/pull/357))

## [0.2.58](https://github.com/gcheliz/gtunnel/compare/v0.2.57...v0.2.58) - 2026-10-30

### Fixed

- *(reset)* a dying run can no longer survive or recreate the config directory ([#355](https://github.com/gcheliz/gtunnel/pull/355))

## [0.2.57](https://github.com/gcheliz/gtunnel/compare/v0.2.56...v0.2.57) - 2026-10-30

### Added

- *(tui)* a small-screen layout that keeps the chart, with scrollable list and logs ([#351](https://github.com/gcheliz/gtunnel/pull/351))

### Fixed

- *(move)* a destination without the instance exits 3, not 1 ([#350](https://github.com/gcheliz/gtunnel/pull/350))

## [0.2.56](https://github.com/gcheliz/gtunnel/compare/v0.2.55...v0.2.56) - 2026-10-30

### Fixed

- *(tui)* read a key typed right after a resize, drive gtunnel demo through a pseudo-terminal ([#346](https://github.com/gcheliz/gtunnel/pull/346))

## [0.2.55](https://github.com/gcheliz/gtunnel/compare/v0.2.54...v0.2.55) - 2026-10-30

### Fixed

- *(zone)* warn once when the zone lookup falls back to the default ([#343](https://github.com/gcheliz/gtunnel/pull/343))

## [0.2.54](https://github.com/gcheliz/gtunnel/compare/v0.2.53...v0.2.54) - 2026-10-30

### Added

- *(zone)* look an instance's zone up by name instead of asking for it ([#339](https://github.com/gcheliz/gtunnel/pull/339))

## [0.2.53](https://github.com/gcheliz/gtunnel/compare/v0.2.52...v0.2.53) - 2026-10-30

### Added

- *(tui)* hide stopped instances and GKE nodes in the picker, rank IAP-reachable ones first ([#338](https://github.com/gcheliz/gtunnel/pull/338))

### Fixed

- wait for runs before reset removes the config, refuse edits on a busy row up front ([#335](https://github.com/gcheliz/gtunnel/pull/335))

## [0.2.52](https://github.com/gcheliz/gtunnel/compare/v0.2.51...v0.2.52) - 2026-10-30

### Fixed

- *(run)* keep a run alive on unreadable output, fail on a gcloud error, honour a second Ctrl+C ([#332](https://github.com/gcheliz/gtunnel/pull/332))

## [0.2.51](https://github.com/gcheliz/gtunnel/compare/v0.2.50...v0.2.51) - 2026-10-30

### Fixed

- *(run)* record the owner and stop orphaned runs from the watchdog ([#330](https://github.com/gcheliz/gtunnel/pull/330))

## [0.2.50](https://github.com/gcheliz/gtunnel/compare/v0.2.49...v0.2.50) - 2026-10-30

### Fixed

- restore the terminal on SIGTERM, dedupe logs -f, surface an unreadable run dir ([#328](https://github.com/gcheliz/gtunnel/pull/328))

## [0.2.49](https://github.com/gcheliz/gtunnel/compare/v0.2.48...v0.2.49) - 2026-10-30

### Fixed

- *(tui)* key operations and rounds by account and finish them before quitting ([#329](https://github.com/gcheliz/gtunnel/pull/329))

## [0.2.48](https://github.com/gcheliz/gtunnel/compare/v0.2.47...v0.2.48) - 2026-10-30

### Fixed

- validate TUI port edits, parse named launchd exit codes, read spaced gcloud flags ([#327](https://github.com/gcheliz/gtunnel/pull/327))

## [0.2.47](https://github.com/gcheliz/gtunnel/compare/v0.2.46...v0.2.47) - 2026-10-30

### Fixed

- reserve before building in expose, re-check in doctor --fix and account remove ([#326](https://github.com/gcheliz/gtunnel/pull/326))

## [0.2.46](https://github.com/gcheliz/gtunnel/compare/v0.2.45...v0.2.46) - 2026-10-30

### Fixed

- *(errors)* give GCP and start failures their kind, and keep the cause in MCP replies ([#324](https://github.com/gcheliz/gtunnel/pull/324))

## [0.2.45](https://github.com/gcheliz/gtunnel/compare/v0.2.44...v0.2.45) - 2026-10-30

### Fixed

- *(fs)* write every service file atomically and replace the binary by rename ([#323](https://github.com/gcheliz/gtunnel/pull/323))

## [0.2.44](https://github.com/gcheliz/gtunnel/compare/v0.2.43...v0.2.44) - 2026-10-30

### Fixed

- *(tui)* no prompt and no print under the alternate screen ([#331](https://github.com/gcheliz/gtunnel/pull/331))

## [0.2.43](https://github.com/gcheliz/gtunnel/compare/v0.2.42...v0.2.43) - 2026-10-30

### Fixed

- *(supervise)* listen for SIGTERM for the whole run and sweep the group after gcloud exits ([#321](https://github.com/gcheliz/gtunnel/pull/321))

## [0.2.42](https://github.com/gcheliz/gtunnel/compare/v0.2.41...v0.2.42) - 2026-10-29

### Fixed

- *(watch)* re-check before acting and hold a lock for the pass ([#325](https://github.com/gcheliz/gtunnel/pull/325))

## [0.2.41](https://github.com/gcheliz/gtunnel/compare/v0.2.40...v0.2.41) - 2026-10-29

### Fixed

- *(tui)* refuse delete, auto-start and start on a run row, and run names in add ([#322](https://github.com/gcheliz/gtunnel/pull/322))

## [0.2.40](https://github.com/gcheliz/gtunnel/compare/v0.2.39...v0.2.40) - 2026-10-29

### Fixed

- *(update)* name the install method's command in the update hint ([#287](https://github.com/gcheliz/gtunnel/pull/287))

## [0.2.39](https://github.com/gcheliz/gtunnel/compare/v0.2.38...v0.2.39) - 2026-10-29

### Fixed

- *(cli)* end quietly when the reader of the output has gone ([#285](https://github.com/gcheliz/gtunnel/pull/285))

## [0.2.38](https://github.com/gcheliz/gtunnel/compare/v0.2.37...v0.2.38) - 2026-10-29

### Documentation

- *(usage)* show the login check on a command that runs it ([#280](https://github.com/gcheliz/gtunnel/pull/280))
- trim every page to what the reader needs ([#278](https://github.com/gcheliz/gtunnel/pull/278))
- give each repeated topic one owner and link to it elsewhere ([#277](https://github.com/gcheliz/gtunnel/pull/277))
- lead every page with what it gives and fix the facts that were wrong ([#276](https://github.com/gcheliz/gtunnel/pull/276))
- *(install)* say what Homebrew pulls on an older Linux and which platforms ran ([#275](https://github.com/gcheliz/gtunnel/pull/275))
- *(install)* correct the tap and trust steps for Homebrew 6 ([#274](https://github.com/gcheliz/gtunnel/pull/274))
- *(install)* say how to trust the tap and what to do with a second copy ([#273](https://github.com/gcheliz/gtunnel/pull/273))

### Fixed

- *(tui)* show health as its icon, read stopped during a stop, fill the demo logs ([#283](https://github.com/gcheliz/gtunnel/pull/283))

## [0.2.37](https://github.com/gcheliz/gtunnel/compare/v0.2.36...v0.2.37) - 2026-10-29

### Documentation

- *(install)* add the Homebrew tap as a way to install ([#269](https://github.com/gcheliz/gtunnel/pull/269))

### Fixed

- *(list)* report health stopped for a stopped tunnel ([#272](https://github.com/gcheliz/gtunnel/pull/272))

## [0.2.36](https://github.com/gcheliz/gtunnel/compare/v0.2.35...v0.2.36) - 2026-10-29

### Added

- *(update)* make the check interval a setting and say when a check fails ([#268](https://github.com/gcheliz/gtunnel/pull/268))

### Documentation

- *(gcp)* add the GCP requirements page and two load diagrams ([#266](https://github.com/gcheliz/gtunnel/pull/266))
- *(site)* drop the lone 14 tools figure from the MCP section ([#265](https://github.com/gcheliz/gtunnel/pull/265))
- *(site)* show the Claude Code and Gemini CLI MCP install commands ([#262](https://github.com/gcheliz/gtunnel/pull/262))

## [0.2.35](https://github.com/gcheliz/gtunnel/compare/v0.2.34...v0.2.35) - 2026-10-29

### Added

- *(config)* the wait for one list probe is a setting ([#260](https://github.com/gcheliz/gtunnel/pull/260))

### Documentation

- *(site)* the hero says what gtunnel does for a reader who has not seen it ([#259](https://github.com/gcheliz/gtunnel/pull/259))

## [0.2.34](https://github.com/gcheliz/gtunnel/compare/v0.2.33...v0.2.34) - 2026-10-29

### Fixed

- *(tui)* the step 3 prompt asks for the target private host ([#258](https://github.com/gcheliz/gtunnel/pull/258))

## [0.2.33](https://github.com/gcheliz/gtunnel/compare/v0.2.32...v0.2.33) - 2026-10-29

### Documentation

- *(usage)* add Homebrew as a way to install Rust ([#252](https://github.com/gcheliz/gtunnel/pull/252))

### Fixed

- *(tui)* pickers cut a long name and show the highlighted one in full ([#253](https://github.com/gcheliz/gtunnel/pull/253))

## [0.2.32](https://github.com/gcheliz/gtunnel/compare/v0.2.31...v0.2.32) - 2026-10-28

### Added

- *(config)* the GCP client's timings, the file lock wait and the fingerprint retries are settings ([#251](https://github.com/gcheliz/gtunnel/pull/251))

## [0.2.31](https://github.com/gcheliz/gtunnel/compare/v0.2.30...v0.2.31) - 2026-10-28

### Added

- *(config)* the log's size cap, trim size, line rate and auth window are settings ([#249](https://github.com/gcheliz/gtunnel/pull/249))

### Documentation

- *(site)* re-record the demos and check the doc blocks against the binary ([#247](https://github.com/gcheliz/gtunnel/pull/247))

## [0.2.30](https://github.com/gcheliz/gtunnel/compare/v0.2.29...v0.2.30) - 2026-10-28

### Added

- *(config)* the watchdog's probe timeout, degraded threshold and interval are settings ([#246](https://github.com/gcheliz/gtunnel/pull/246))

## [0.2.29](https://github.com/gcheliz/gtunnel/compare/v0.2.28...v0.2.29) - 2026-10-28

### Fixed

- *(daemon)* reinstall services whose definition predates the running binary ([#245](https://github.com/gcheliz/gtunnel/pull/245))

## [0.2.28](https://github.com/gcheliz/gtunnel/compare/v0.2.27...v0.2.28) - 2026-10-28

### Added

- *(config)* settings resolve flag, env, file, default; init writes them ([#243](https://github.com/gcheliz/gtunnel/pull/243))

### Documentation

- *(naming)* call the instance a GCP instance in the docs and the site ([#244](https://github.com/gcheliz/gtunnel/pull/244))

## [0.2.27](https://github.com/gcheliz/gtunnel/compare/v0.2.26...v0.2.27) - 2026-10-28

### Changed

- *(naming)* call the instance a GCP instance in src and drop VM ([#240](https://github.com/gcheliz/gtunnel/pull/240))

## [0.2.26](https://github.com/gcheliz/gtunnel/compare/v0.2.25...v0.2.26) - 2026-10-28

### Added

- *(supervise)* stop a job that spends its restart budget ([#237](https://github.com/gcheliz/gtunnel/pull/237))

### Documentation

- *(cli)* flag help names what each value is and shows an example ([#236](https://github.com/gcheliz/gtunnel/pull/236))

## [0.2.25](https://github.com/gcheliz/gtunnel/compare/v0.2.24...v0.2.25) - 2026-10-28

### Added

- *(init)* every init prompt says what it sets and how to find it ([#233](https://github.com/gcheliz/gtunnel/pull/233))

### Documentation

- *(troubleshooting)* explain restarts after the laptop wakes ([#232](https://github.com/gcheliz/gtunnel/pull/232))
- *(decisions)* the supervisor stops a job that spends its restart budget ([#231](https://github.com/gcheliz/gtunnel/pull/231))

## [0.2.24](https://github.com/gcheliz/gtunnel/compare/v0.2.23...v0.2.24) - 2026-10-28

### Added

- *(init)* a bare gtunnel on a fresh install runs the configuration picker ([#227](https://github.com/gcheliz/gtunnel/pull/227))

## [0.2.23](https://github.com/gcheliz/gtunnel/compare/v0.2.22...v0.2.23) - 2026-10-28

### Fixed

- *(init)* a gcloud configuration without a zone no longer blocks init ([#226](https://github.com/gcheliz/gtunnel/pull/226))

## [0.2.22](https://github.com/gcheliz/gtunnel/compare/v0.2.21...v0.2.22) - 2026-10-28

### Documentation

- credential file is read only by doctor, and the panic guard covers unwinding builds ([#221](https://github.com/gcheliz/gtunnel/pull/221))

### Fixed

- *(tui)* account add, remove and select no longer print onto the drawn screen ([#223](https://github.com/gcheliz/gtunnel/pull/223))

## [0.2.21](https://github.com/gcheliz/gtunnel/compare/v0.2.20...v0.2.21) - 2026-10-28

### Fixed

- *(doctor)* auth advice names the configuration and keeps gcloud's reason ([#220](https://github.com/gcheliz/gtunnel/pull/220))

## [0.2.20](https://github.com/gcheliz/gtunnel/compare/v0.2.19...v0.2.20) - 2026-10-28

### Fixed

- *(account)* refuse empty configuration and bad zone, and name tunnels an edit leaves stale ([#219](https://github.com/gcheliz/gtunnel/pull/219))

## [0.2.19](https://github.com/gcheliz/gtunnel/compare/v0.2.18...v0.2.19) - 2026-10-28

### Fixed

- *(config)* a config.toml that fails to load is never overwritten ([#218](https://github.com/gcheliz/gtunnel/pull/218))

## [0.2.18](https://github.com/gcheliz/gtunnel/compare/v0.2.17...v0.2.18) - 2026-10-28

### Documentation

- *(decisions)* restarts belong to launchd and systemd, and readiness is a file ([#210](https://github.com/gcheliz/gtunnel/pull/210))

### Fixed

- *(tui)* bound each metrics target so one slow tunnel cannot freeze the round ([#211](https://github.com/gcheliz/gtunnel/pull/211))

## [0.2.17](https://github.com/gcheliz/gtunnel/compare/v0.2.16...v0.2.17) - 2026-10-28

### Fixed

- *(auth)* quote the chmod advice and keep gcloud's reason for a credential file ([#207](https://github.com/gcheliz/gtunnel/pull/207))

## [0.2.16](https://github.com/gcheliz/gtunnel/compare/v0.2.15...v0.2.16) - 2026-10-28

### Added

- *(account)* run gtunnel in CI with a per-account credential file ([#206](https://github.com/gcheliz/gtunnel/pull/206))

## [0.2.15](https://github.com/gcheliz/gtunnel/compare/v0.2.14...v0.2.15) - 2026-10-28

### Fixed

- *(tui)* a panicked round no longer freezes the metrics ([#205](https://github.com/gcheliz/gtunnel/pull/205))

## [0.2.14](https://github.com/gcheliz/gtunnel/compare/v0.2.13...v0.2.14) - 2026-10-28

### Documentation

- *(architecture)* show that expired credentials stop the restart loop ([#203](https://github.com/gcheliz/gtunnel/pull/203))

### Performance

- *(tui)* probe each tunnel once per tick ([#204](https://github.com/gcheliz/gtunnel/pull/204))

## [0.2.13](https://github.com/gcheliz/gtunnel/compare/v0.2.12...v0.2.13) - 2026-10-28

### Fixed

- *(list)* say auth expired in plain list and get_status ([#200](https://github.com/gcheliz/gtunnel/pull/200))

## [0.2.12](https://github.com/gcheliz/gtunnel/compare/v0.2.11...v0.2.12) - 2026-10-28

### Fixed

- *(supervise)* stop restarting a tunnel whose credentials expired ([#197](https://github.com/gcheliz/gtunnel/pull/197))

## [0.2.11](https://github.com/gcheliz/gtunnel/compare/v0.2.10...v0.2.11) - 2026-10-28

### Fixed

- *(tui)* collect the metrics round in parallel ([#195](https://github.com/gcheliz/gtunnel/pull/195))

## [0.2.10](https://github.com/gcheliz/gtunnel/compare/v0.2.9...v0.2.10) - 2026-10-27

### Documentation

- *(readme)* add an architecture section before the topology diagram ([#194](https://github.com/gcheliz/gtunnel/pull/194))

## [0.2.9](https://github.com/gcheliz/gtunnel/compare/v0.2.8...v0.2.9) - 2026-10-27

### Fixed

- *(tui)* stop a run with S and refuse R on one ([#190](https://github.com/gcheliz/gtunnel/pull/190))

## [0.2.8](https://github.com/gcheliz/gtunnel/compare/v0.2.7...v0.2.8) - 2026-10-27

### Fixed

- *(tui)* describe every form field and pick the configuration and zone ([#188](https://github.com/gcheliz/gtunnel/pull/188))

## [0.2.7](https://github.com/gcheliz/gtunnel/compare/v0.2.6...v0.2.7) - 2026-10-27

### Documentation

- *(site)* fix the phone layout and re-record the run, access and MCP demos ([#183](https://github.com/gcheliz/gtunnel/pull/183))

## [0.2.6](https://github.com/gcheliz/gtunnel/compare/v0.2.5...v0.2.6) - 2026-10-27

### Documentation

- *(readme)* open with the TUI, CLI and MCP, and re-record the TUI demo ([#180](https://github.com/gcheliz/gtunnel/pull/180))

## [0.2.5](https://github.com/gcheliz/gtunnel/compare/v0.2.4...v0.2.5) - 2026-10-27

### Documentation

- *(readme)* slim the README, link the docs, fix site and architecture drift ([#178](https://github.com/gcheliz/gtunnel/pull/178))

## [0.2.4](https://github.com/gcheliz/gtunnel/compare/v0.2.3...v0.2.4) - 2026-10-27

### Fixed

- *(tui)* scale the metrics chart on the median so a steady tunnel is not a solid block ([#176](https://github.com/gcheliz/gtunnel/pull/176))

## [0.2.3](https://github.com/gcheliz/gtunnel/compare/v0.2.2...v0.2.3) - 2026-10-27

### Fixed

- *(expose)* check the local target before creating the route ([#173](https://github.com/gcheliz/gtunnel/pull/173))

## [0.2.2](https://github.com/gcheliz/gtunnel/compare/v0.2.1...v0.2.2) - 2026-10-27

### Added

- *(mcp)* serve revision 2026-07-28 statelessly beside initialize ([#169](https://github.com/gcheliz/gtunnel/pull/169))

## [0.2.1](https://github.com/gcheliz/gtunnel/compare/v0.2.0...v0.2.1) - 2026-10-27

### Added

- *(doctor)* refuse and report unknown keys in config.toml and tunnels.toml ([#163](https://github.com/gcheliz/gtunnel/pull/163))

### Fixed

- *(expose)* refuse port 0 in host:port, require started in the schema ([#167](https://github.com/gcheliz/gtunnel/pull/167))

## [0.2.0](https://github.com/gcheliz/gtunnel/compare/v0.1.64...v0.2.0) - 2026-10-27

### Fixed

- *(cli)* [**breaking**] refusals of the request exit 2 (usage) ([#166](https://github.com/gcheliz/gtunnel/pull/166))

## [0.1.64](https://github.com/gcheliz/gtunnel/compare/v0.1.63...v0.1.64) - 2026-10-27

### Fixed

- *(fsutil)* back off from 1 ms when polling a held lock ([#164](https://github.com/gcheliz/gtunnel/pull/164))

## [0.1.63](https://github.com/gcheliz/gtunnel/compare/v0.1.62...v0.1.63) - 2026-10-27

### Added

- *(account)* pick the GCP project from a list, typing stays the fallback ([#160](https://github.com/gcheliz/gtunnel/pull/160))

## [0.1.62](https://github.com/gcheliz/gtunnel/compare/v0.1.61...v0.1.62) - 2026-10-27

### Added

- *(mcp)* expose tool, private unless public is true ([#159](https://github.com/gcheliz/gtunnel/pull/159))

## [0.1.61](https://github.com/gcheliz/gtunnel/compare/v0.1.60...v0.1.61) - 2026-10-27

### Added

- *(mcp)* rename and move tools ([#157](https://github.com/gcheliz/gtunnel/pull/157))

## [0.1.60](https://github.com/gcheliz/gtunnel/compare/v0.1.59...v0.1.60) - 2026-10-27

### Documentation

- *(architecture)* retitle the public-mode diagram as expose mode ([#156](https://github.com/gcheliz/gtunnel/pull/156))

## [0.1.59](https://github.com/gcheliz/gtunnel/compare/v0.1.58...v0.1.59) - 2026-10-27

### Added

- *(mcp)* negotiate up to 2025-11-25, annotate every tool, structured output ([#151](https://github.com/gcheliz/gtunnel/pull/151))

## [0.1.58](https://github.com/gcheliz/gtunnel/compare/v0.1.57...v0.1.58) - 2026-10-27

### Documentation

- *(usage)* name the two modes, split the command reference, add the config reference ([#155](https://github.com/gcheliz/gtunnel/pull/155))

## [0.1.57](https://github.com/gcheliz/gtunnel/compare/v0.1.56...v0.1.57) - 2026-10-27

### Documentation

- *(site)* restyle the CLI and MCP rows, compact the code blocks, shrink the GitHub button ([#145](https://github.com/gcheliz/gtunnel/pull/145))

## [0.1.56](https://github.com/gcheliz/gtunnel/compare/v0.1.55...v0.1.56) - 2026-10-27

### Documentation

- *(site)* drop the section menu, balance the CLI and MCP rows, add copy buttons ([#144](https://github.com/gcheliz/gtunnel/pull/144))

## [0.1.55](https://github.com/gcheliz/gtunnel/compare/v0.1.54...v0.1.55) - 2026-10-27

### Fixed

- *(doctor)* report a service that no tunnel record owns ([#143](https://github.com/gcheliz/gtunnel/pull/143))

## [0.1.54](https://github.com/gcheliz/gtunnel/compare/v0.1.53...v0.1.54) - 2026-10-27

### Added

- *(list)* report health and local_port in list --json and gtunnel_list_tunnels ([#142](https://github.com/gcheliz/gtunnel/pull/142))

## [0.1.53](https://github.com/gcheliz/gtunnel/compare/v0.1.52...v0.1.53) - 2026-10-27

### Fixed

- *(config)* reserve the account names run and run-* ([#139](https://github.com/gcheliz/gtunnel/pull/139))

## [0.1.52](https://github.com/gcheliz/gtunnel/compare/v0.1.51...v0.1.52) - 2026-10-27

### Fixed

- *(mcp)* start each run's log fresh instead of appending to the last one ([#138](https://github.com/gcheliz/gtunnel/pull/138))

## [0.1.51](https://github.com/gcheliz/gtunnel/compare/v0.1.50...v0.1.51) - 2026-10-27

### Documentation

- *(site)* promote the tool as TUI, CLI and MCP and drop the architecture tour ([#141](https://github.com/gcheliz/gtunnel/pull/141))

### Fixed

- *(state)* refuse a tunnel whose file names another account's tunnel already holds ([#140](https://github.com/gcheliz/gtunnel/pull/140))

## [0.1.50](https://github.com/gcheliz/gtunnel/compare/v0.1.49...v0.1.50) - 2026-10-27

### Fixed

- *(tui)* refuse a config edit that puts a tunnel on a live run's port ([#131](https://github.com/gcheliz/gtunnel/pull/131))

## [0.1.49](https://github.com/gcheliz/gtunnel/compare/v0.1.48...v0.1.49) - 2026-10-27

### Fixed

- *(mcp)* make gtunnel://accounts match gtunnel_list_accounts, and bring the docs and skill in line with the code ([#129](https://github.com/gcheliz/gtunnel/pull/129))

## [0.1.48](https://github.com/gcheliz/gtunnel/compare/v0.1.47...v0.1.48) - 2026-10-27

### Fixed

- *(add)* refuse or skip a local port a live run holds ([#128](https://github.com/gcheliz/gtunnel/pull/128))

## [0.1.47](https://github.com/gcheliz/gtunnel/compare/v0.1.46...v0.1.47) - 2026-10-27

### Fixed

- *(run)* let stop, get_status and get_logs find a live run, and add gtunnel logs for runs ([#127](https://github.com/gcheliz/gtunnel/pull/127))

## [0.1.46](https://github.com/gcheliz/gtunnel/compare/v0.1.45...v0.1.46) - 2026-10-27

### Fixed

- *(run)* stop the whole process group, enforce the TTL on the wall clock, and refuse a name or port in use ([#123](https://github.com/gcheliz/gtunnel/pull/123))

## [0.1.45](https://github.com/gcheliz/gtunnel/compare/v0.1.44...v0.1.45) - 2026-10-27

### Fixed

- *(tui)* lock the config editor's save and refuse a file that changed under it ([#122](https://github.com/gcheliz/gtunnel/pull/122))

## [0.1.44](https://github.com/gcheliz/gtunnel/compare/v0.1.43...v0.1.44) - 2026-10-27

### Fixed

- *(mcp)* answer no notification, refuse bad ports, give every tool error a kind, and let list run logged out ([#121](https://github.com/gcheliz/gtunnel/pull/121))

## [0.1.43](https://github.com/gcheliz/gtunnel/compare/v0.1.42...v0.1.43) - 2026-10-27

### Added

- *(tui)* show a run's stop time, and refresh the demo and its recordings ([#112](https://github.com/gcheliz/gtunnel/pull/112))

## [0.1.42](https://github.com/gcheliz/gtunnel/compare/v0.1.41...v0.1.42) - 2026-10-27

### Changed

- split a library crate out of the binary and move the ui tests beside ui.rs ([#113](https://github.com/gcheliz/gtunnel/pull/113))

## [0.1.41](https://github.com/gcheliz/gtunnel/compare/v0.1.40...v0.1.41) - 2026-10-27

### Added

- *(cli)* report json errors with a kind and an exit code ([#111](https://github.com/gcheliz/gtunnel/pull/111))
- *(cli)* add shell completions with tunnel names ([#110](https://github.com/gcheliz/gtunnel/pull/110))
- *(tui)* name expired credentials and crash loops in tunnel health ([#109](https://github.com/gcheliz/gtunnel/pull/109))

## [0.1.40](https://github.com/gcheliz/gtunnel/compare/v0.1.39...v0.1.40) - 2026-10-27

### Performance

- cut the release binary from 11.5 MB to 4.2 MB ([#106](https://github.com/gcheliz/gtunnel/pull/106))

## [0.1.39](https://github.com/gcheliz/gtunnel/compare/v0.1.38...v0.1.39) - 2026-10-27

### Fixed

- *(probe)* a silent close after a logged refusal is not healthy ([#108](https://github.com/gcheliz/gtunnel/pull/108))

## [0.1.38](https://github.com/gcheliz/gtunnel/compare/v0.1.37...v0.1.38) - 2026-10-27

### Fixed

- *(state)* reject instance and host values that gcloud would read as flags ([#105](https://github.com/gcheliz/gtunnel/pull/105))

## [0.1.37](https://github.com/gcheliz/gtunnel/compare/v0.1.36...v0.1.37) - 2026-10-27

### Fixed

- *(supervise)* give gcloud time to exit before killing it ([#104](https://github.com/gcheliz/gtunnel/pull/104))

## [0.1.36](https://github.com/gcheliz/gtunnel/compare/v0.1.35...v0.1.36) - 2026-10-27

### Added

- *(mcp)* open ephemeral tunnels from agents, and compact the landing page ([#90](https://github.com/gcheliz/gtunnel/pull/90))

## [0.1.35](https://github.com/gcheliz/gtunnel/compare/v0.1.34...v0.1.35) - 2026-10-27

### Fixed

- *(daemon)* toggling auto-start no longer restarts a running tunnel ([#89](https://github.com/gcheliz/gtunnel/pull/89))

## [0.1.34](https://github.com/gcheliz/gtunnel/compare/v0.1.33...v0.1.34) - 2026-10-27

### Documentation

- add the architecture tour, tighten spacing, render every diagram with the infra pipeline ([#87](https://github.com/gcheliz/gtunnel/pull/87))

## [0.1.33](https://github.com/gcheliz/gtunnel/compare/v0.1.32...v0.1.33) - 2026-10-27

### Fixed

- *(notify)* announce every tunnel's state changes once ([#86](https://github.com/gcheliz/gtunnel/pull/86))

## [0.1.32](https://github.com/gcheliz/gtunnel/compare/v0.1.31...v0.1.32) - 2026-10-27

### Fixed

- *(tui)* show a starting state and wait for readiness ([#85](https://github.com/gcheliz/gtunnel/pull/85))

## [0.1.31](https://github.com/gcheliz/gtunnel/compare/v0.1.30...v0.1.31) - 2026-10-27

### Fixed

- *(logs)* record each connection with its source port ([#84](https://github.com/gcheliz/gtunnel/pull/84))

## [0.1.30](https://github.com/gcheliz/gtunnel/compare/v0.1.29...v0.1.30) - 2026-10-27

### Changed

- bound every remaining subprocess ([#83](https://github.com/gcheliz/gtunnel/pull/83))

## [0.1.29](https://github.com/gcheliz/gtunnel/compare/v0.1.28...v0.1.29) - 2026-10-27

### Documentation

- refresh the architecture diagrams and README features ([#78](https://github.com/gcheliz/gtunnel/pull/78))

## [0.1.28](https://github.com/gcheliz/gtunnel/compare/v0.1.27...v0.1.28) - 2026-10-27

### Fixed

- *(tui)* run one poller round at a time and bound every subprocess ([#77](https://github.com/gcheliz/gtunnel/pull/77))

## [0.1.27](https://github.com/gcheliz/gtunnel/compare/v0.1.26...v0.1.27) - 2026-10-27

### Added

- *(tui)* show an available update in the header rule ([#76](https://github.com/gcheliz/gtunnel/pull/76))

## [0.1.26](https://github.com/gcheliz/gtunnel/compare/v0.1.25...v0.1.26) - 2026-10-27

### Fixed

- *(state)* write tunnels.toml, config.toml and run records atomically under a lock ([#74](https://github.com/gcheliz/gtunnel/pull/74))

## [0.1.25](https://github.com/gcheliz/gtunnel/compare/v0.1.24...v0.1.25) - 2026-10-27

### Fixed

- *(mcp)* keep stdout pure and report failed starts as failures ([#71](https://github.com/gcheliz/gtunnel/pull/71))

## [0.1.24](https://github.com/gcheliz/gtunnel/compare/v0.1.23...v0.1.24) - 2026-10-27

### Fixed

- *(daemon)* treat a systemd unit that is not loaded as already stopped

## [0.1.23](https://github.com/gcheliz/gtunnel/compare/v0.1.22...v0.1.23) - 2026-10-27

### Documentation

- document installing Rust and a C compiler for the cargo routes ([#72](https://github.com/gcheliz/gtunnel/pull/72))

## [0.1.22](https://github.com/gcheliz/gtunnel/compare/v0.1.21...v0.1.22) - 2026-10-27

### Documentation

- link the landing page from the README and require conventional PR titles ([#70](https://github.com/gcheliz/gtunnel/pull/70))

## [0.1.21](https://github.com/gcheliz/gtunnel/compare/v0.1.20...v0.1.21) - 2026-10-27

### Documentation

- re-record the TUI demo with the edit, add and accounts flows ([#67](https://github.com/gcheliz/gtunnel/pull/67))

## [0.1.20](https://github.com/gcheliz/gtunnel/compare/v0.1.19...v0.1.20) - 2026-10-27

### Fixed

- *(discovery)* stop listing a tunnel gtunnel just started as unmanaged ([#66](https://github.com/gcheliz/gtunnel/pull/66))

## [0.1.19](https://github.com/gcheliz/gtunnel/compare/v0.1.18...v0.1.19) - 2026-10-27

### Fixed

- *(release)* ship lockfile-only dependency updates ([#64](https://github.com/gcheliz/gtunnel/pull/64))

## [0.1.18](https://github.com/gcheliz/gtunnel/compare/v0.1.17...v0.1.18) - 2026-10-27

### Documentation

- sync version references to 0.1.17 and check them in CI ([#52](https://github.com/gcheliz/gtunnel/pull/52))

## [0.1.17](https://github.com/gcheliz/gtunnel/compare/v0.1.16...v0.1.17) - 2026-10-26

### Added

- *(mcp)* add native mcp server, agent skill, and json cli output ([#50](https://github.com/gcheliz/gtunnel/pull/50))

## [0.1.16](https://github.com/gcheliz/gtunnel/compare/v0.1.15...v0.1.16) - 2026-10-26

### Added

- *(cli)* allow all config commands unauthenticated and deduplicate workflows ([#48](https://github.com/gcheliz/gtunnel/pull/48))

## [0.1.15](https://github.com/gcheliz/gtunnel/compare/v0.1.14...v0.1.15) - 2026-10-26

### Added

- *(update)* allow self-update to replace cargo-installed binaries directly

### Documentation

- clarify that cargo installs self-update directly

## [0.1.14](https://github.com/gcheliz/gtunnel/compare/v0.1.13...v0.1.14) - 2026-10-26

### Added

- *(ci)* automate release tagging and include version in release assets ([#45](https://github.com/gcheliz/gtunnel/pull/45))

## [0.1.13](https://github.com/gcheliz/gtunnel/compare/v0.1.12...v0.1.13) - 2026-10-25

### Added

- *(cli)* standardize command error messages and document interactive prompt flows ([#43](https://github.com/gcheliz/gtunnel/pull/43))

## [0.1.12](https://github.com/gcheliz/gtunnel/compare/v0.1.11...v0.1.12) - 2026-10-25

### Fixed

- improve health check responsiveness and log filtering ([#42](https://github.com/gcheliz/gtunnel/pull/42))

## [0.1.10](https://github.com/gcheliz/gtunnel/compare/v0.1.9...v0.1.10) - 2026-10-25

### Fixed

- *(tui)* eliminate latency sparkline gaps, log origin endpoints, and add copy toast ([#36](https://github.com/gcheliz/gtunnel/pull/36))

## [0.1.9](https://github.com/gcheliz/gtunnel/compare/v0.1.8...v0.1.9) - 2026-10-25

### Documentation

- synchronize commands, keybindings, paths, and output references ([#34](https://github.com/gcheliz/gtunnel/pull/34))

## [0.1.8](https://github.com/gcheliz/gtunnel/compare/v0.1.7...v0.1.8) - 2026-10-25

### Added

- *(tui)* bind l to copy tunnel log to clipboard and quiet jump probe noise ([#32](https://github.com/gcheliz/gtunnel/pull/32))

## [0.1.7](https://github.com/gcheliz/gtunnel/compare/v0.1.6...v0.1.7) - 2026-10-25

### Fixed

- *(logfilter)* drop iap websocket teardown tracebacks and routine connection info lines ([#30](https://github.com/gcheliz/gtunnel/pull/30))

## [0.1.6](https://github.com/gcheliz/gtunnel/compare/v0.1.5...v0.1.6) - 2026-10-25

### Fixed

- *(tui)* guard s and S keybindings by status and ignore stdin proxy commands ([#28](https://github.com/gcheliz/gtunnel/pull/28))

## [0.1.5](https://github.com/gcheliz/gtunnel/compare/v0.1.4...v0.1.5) - 2026-10-25

### Added

- *(daemon)* supervise tunnel processes with output filtering and timestamping ([#26](https://github.com/gcheliz/gtunnel/pull/26))

## [0.1.4](https://github.com/gcheliz/gtunnel/compare/v0.1.3...v0.1.4) - 2026-10-25

### Added

- ~/.gtunnel configuration directory path with backwards-compatible fallback ([#22](https://github.com/gcheliz/gtunnel/pull/22))
- SESSION PATHS section in the `?` help modal displaying active config, tunnels, and logs paths ([#22](https://github.com/gcheliz/gtunnel/pull/22))
- interactive confirmation prompt before launching `gcloud auth login` on unauthenticated startup ([#22](https://github.com/gcheliz/gtunnel/pull/22))

## [0.1.3](https://github.com/gcheliz/gtunnel/compare/v0.1.2...v0.1.3) - 2026-10-24

### Added

- a watchdog that restarts a wedged tunnel, and gives up before it thrashes ([#20](https://github.com/gcheliz/gtunnel/pull/20))
- tunnels get descriptor headroom and explain it when they run out ([#19](https://github.com/gcheliz/gtunnel/pull/19))
- one log level for the tunnels and the gcloud calls ([#18](https://github.com/gcheliz/gtunnel/pull/18))

## [0.1.2](https://github.com/gcheliz/gtunnel/compare/v0.1.1...v0.1.2) - 2026-10-24

### Added

- the help frame names the running version, so a bug report can quote it
  ([#15](https://github.com/gcheliz/gtunnel/pull/15)). Press `?`; the title reads
  `Keys - gtunnel 0.1.2`, and sheds the version below 28 columns rather than clipping it

### Documentation

- the version table described a 1.0 project, and this one is not ([#17](https://github.com/gcheliz/gtunnel/pull/17))
- the cargo uninstall recovery hits an ambiguity, so show it ([#16](https://github.com/gcheliz/gtunnel/pull/16))
- installation recovery, a full config reference, and a release tag gate ([#15](https://github.com/gcheliz/gtunnel/pull/15))
- say what gh attestation verify actually prints ([#11](https://github.com/gcheliz/gtunnel/pull/11))

## v0.1.1 - 2026-10-24

### Installing and proving what you installed

- **Every release asset carries a build attestation** - a `.sha256` proves the download
  arrived intact, not who produced it, since whoever could swap the tarball could swap the
  checksum beside it. Each asset is now signed by GitHub with a short-lived certificate
  tied to this repository's Release workflow and recorded in a public transparency log.
  `gh attestation verify <file> -o gcheliz` checks it. Attestations begin with v0.1.1.
- **`gtunnel update` no longer sends a cargo install to crates.io** - the `gtunnel` name
  there belongs to an unrelated crate by another author, so the old hint would have
  installed a stranger's binary over this one. It now names the repository:
  `cargo install --git https://github.com/gcheliz/gtunnel --force`.
- **Installing is documented for four routes** - a published binary through a URL that
  carries no version, `cargo install --git`, `cargo install --path`, and `gtunnel update`.

### Project documentation

- **`CONTRIBUTING.md`** - what to install, the six CI gate commands to run before pushing,
  the placeholder rule for a public repository and how to capture real output without
  breaking it, commit and pull-request conventions, and which doc a given change updates.
- **`docs/decisions.md`** - thirteen records covering the choices that would otherwise be
  re-litigated: no stored credential, Access mode creating nothing in GCP, user services
  instead of a daemon, ports derived from the tunnel name, naming, the operator owning the
  relay firewall, static musl builds, the shape of the CI gate, and squash-only merges.
  Each says what it costs, not only what was chosen.

### Accounts, renaming and moving

- **`gtunnel rename` renames the tunnel, not just the record** - a name is also the
  service label and the log filename, so editing `tunnels.toml` by hand left an orphaned
  plist and a log nothing would ever write to again. The command stops the tunnel,
  rewrites the record, carries the log across and reinstalls the service under the new
  label. A rename that fails partway says where the tunnel actually is rather than
  leaving the caller to guess.
- **`gtunnel move` hands an Access tunnel to another account** - the global `--account`
  names the source and `--to` the destination, so neither is inferred from the default.
  It verifies the destination before the first write: an instance that does not exist
  over there costs nothing and the error says `nothing was changed`. Public tunnels are
  refused, because a URL, its certificate and its load-balancer rules belong to the
  account that created them, and a half-moved URL serves a 503 to real visitors.
- **A name collision asks instead of cancelling** - names are scoped per account, so a
  move can land on one the destination already holds. The CLI takes `--rename` up front;
  the TUI does not require it to be predicted, and advances to a rename prompt with the
  reason on screen at the moment the clash is found.
- **An account that still holds tunnels cannot be removed** - and the refusal names both
  ways out, `gtunnel delete` and `gtunnel move`, rather than offering a force flag. The
  flag would strand installed services whose account no longer exists, which is the one
  state nothing in gtunnel can clean up afterwards. The TUI's accounts page answers to
  the same rule: its own confirmation does not override it.
- **Accounts are managed from the TUI** - `:`, the shift of `;`, opens a page that adds,
  edits, selects and removes them. `account add` reads the project and default zone from
  an existing gcloud configuration, so an account is never a second place to type what
  gcloud already knows.
- **`e` chooses what to edit** - target and ports, rename, or move, rather than spending
  two more top-level keys on a keymap that already binds 31. Both new options refuse an
  unmanaged row and an ephemeral run that has not been imported with `m`.

### Logs

- **A running tunnel's log now shows that it is running** - four causes, all fixed.
  gcloud is Python and block-buffered its stdout into the redirected log, so lines
  arrived 4 KiB at a time or at exit; `PYTHONUNBUFFERED=1` ends that. gcloud's
  per-connection lines are `log.info` and the default verbosity is `warning`, so a tunnel
  carrying traffic said nothing; every command now passes `--verbosity=info`. The `--via`
  and public kinds run `gcloud compute ssh`, which gcloud's verbosity does not govern and
  which is silent once connected, so both also pass `-o LogLevel=DEBUG` - not `VERBOSE`,
  which is one level short of every line worth having. `New connection from`, the line
  this was measured against, does not exist anywhere in gcloud 590.0.0.
- **gtunnel writes what only gtunnel knows** - the connection count it already measured
  every 5 seconds reached the Metrics pane and nothing else. A change in it is now a log
  line, and so is a health transition, which until now existed only as a desktop
  notification that was gone once dismissed. Both are written only while a gtunnel
  process runs, and `docs/troubleshooting.md` says so: a gap in the counts means nobody
  was watching, not that the tunnel was idle. The count is an Access and jump fact - a
  public tunnel has no local port of its own to count - and the same document says that
  too. An op that empties a log also drops the count that belonged to it, so a restart
  no longer reports a drop that nobody caused.
- **A talkative log is a bounded one** - past 8 MiB gtunnel keeps the last 2 MiB and
  says on the first line what it dropped, and the TUI reads the end of a log rather
  than all of it to show the last 200 lines.

### TUI

- **A config page** - `C` shows `config.toml` and `tunnels.toml` with their paths, and `e`
  edits the one on screen in `$VISUAL`, then `$EDITOR`, then `vi`. The editor works on a
  private copy, which replaces the real file only after it parses and passes the rules
  `gtunnel add` enforces; a refused edit keeps its copy for `e` to reopen and `Esc` to
  discard. A saved edit reloads accounts, projects, the relay and the list without
  restarting, and says what changed. Running tunnels keep their old settings until `R`.

- **`;` switches the relay with the account** - the Public flow and the domain picker
  followed the account the TUI started on, so an account without a relay offered one and an
  account with one refused it. Both now change with the name and the project.

### Fixed

- **A recorded pid of 0 is never signalled** - `kill(0)` signals the caller's own process
  group. A run record holding pid 0, or one too large for a pid, now reads as not running
  and `reset` and the import handover skip it; a run whose `gcloud` has no pid fails to
  start instead of recording one.

### Build

- **A lint policy in `Cargo.toml`** - `unsafe_op_in_unsafe_fn`,
  `clippy::undocumented_unsafe_blocks`, `dbg_macro`, `todo` and `unimplemented` are
  declared once under `[lints]`, so CI and editors agree. Every `unsafe` block now carries
  the invariant that makes it sound.
- **Workflow linting in CI** - a `zizmor` job checks the workflows for template injection,
  broad permissions, unpinned actions and persisted credentials. Checkouts no longer keep
  the token, CI jobs are read-only, and the release build no longer restores a build cache.

### Documentation

- **The README points at `docs/` instead of restating it** - it carried the full keymap, the
  full command map and the full error table, three tables that `docs/usage.md` and
  `docs/troubleshooting.md` already own, and each copy drifted on its own schedule. The
  README now orients a newcomer and links; every table it dropped exists in exactly one file
  under `docs/`. 243 lines down to 149.
- **Four diagrams for what the prose only described** - `docs/architecture.md` gains the jump
  tunnel's second hop (`compute ssh --tunnel-through-iap`, which is a different command from
  `start-iap-tunnel` and has no local listener, so nothing prints `Listening on port`), what
  `relay up` creates against what `relay down` removes and in which order, the service
  lifecycle across launchd and systemd with the two asymmetric auto-start switches, and the
  account model that lets `kubernetes` exist once per account.
- **The banner keeps the mark's gaps** - `docs/logo.svg` drew each braille dot as a
  rounded square as wide as the pitch, so the squares met, the full-width row merged into
  the cloud, and the mark read as one bottom-heavy blob. It is one circle per dot again, as
  braille separates them: the gaps are what keep that row reading as a line behind the
  cloud. The silhouette is unchanged - all 48 dots and their hues are still pinned to
  `mark_dots()` - and `docs/logo.png` is re-rendered from it.
- **The fallback lost its notch** - `MARK_FALLBACK` carried `▀` at its centre, an
  upper-half block in a shape that has no cell with empty lower rows, so it drew a hole
  under the cloud on every terminal without braille coverage. Each cell is now the honest
  reduction of its braille cell, and the test bars `▀` by name.
- **A security policy** - `SECURITY.md` names the supported version, routes reports through
  GitHub's private vulnerability reporting rather than a public issue, and says what gtunnel
  holds: no stored credential, a fresh token per call from `gcloud auth print-access-token`,
  and nothing secret in `config.toml` or `tunnels.toml`.
- **Badges that a private repository can back** - the CI badge is gone. GitHub's image proxy
  fetches anonymously, so every dynamic badge against a private repository renders as a broken
  image; the workflow badge returned 404 while the static ones returned 200. In its place are
  static badges for the version, the Rust floor, the edition, the security policy and the
  licence, and a line naming what CI actually runs. **The version badge is written by hand and
  has to be bumped in `README.md` as part of cutting a release.** The CI badge can come back
  the day the repository goes public.

## v0.1.0 - 2026-10-24

First release. gtunnel is a port of [ytunnel](https://yetidevworks.com/ytunnel) to GCP
Identity-Aware Proxy: the same two modes and the same TUI-first shape, with IAP and a
GCP relay in place of Cloudflare Tunnel.

### Access mode

- **IAP port forwarding as a managed service** - `gtunnel add db my-vm:5432` gives a
  local port that reaches a VM inside the VPC, supervised by launchd or systemd so it
  survives a crash and, optionally, a reboot.
- **Destination groups** - `--dest-group` targets an IAP destination group, which
  reaches a host that is not a compute instance.
- **Stable local ports** - the port is derived from the tunnel name, so the same tunnel
  gets the same port on every machine unless it is taken.
- **Nothing is created in GCP** - Access mode makes no API call beyond reading, so a
  tunnel costs nothing and leaves nothing behind.

### Public mode

- **A public HTTPS URL for a local service** - `gtunnel expose web 3000` publishes
  `https://web.<domain>` through a relay VM, a global load balancer and Cloud DNS.
- **One relay per account** - the VM, load balancer, certificate map and firewall rules
  are created once by `gtunnel relay up` and shared by every tunnel on the account.
- **Managed certificates** - `gtunnel domains add` requests a certificate for the zone
  and its wildcard and writes the DNS authorization record.
- **IAP in front of the load balancer** - `expose --private` requires a Google sign-in
  before a visitor reaches the tunnel.
- **Self-healing routes** - starting a public tunnel re-creates its backend, host rule,
  named port and DNS record if any of them went missing, so `restart` fixes a tunnel
  that comes up serving a 503.

### TUI

- **Live dashboard** - tunnel list, details, logs and metrics in one screen, with
  latency, connection counts and load-balancer request classes.
- **Full lifecycle from the keyboard** - add, edit, start, stop, restart, delete,
  auto-start, copy and open, without leaving the TUI.
- **Unmanaged tunnels** - a `gcloud start-iap-tunnel` process started outside gtunnel
  is listed and read-only. It is named for the target and port it holds rather than
  for its pid, which is the one fact about it nothing can act on; the pid returns only
  to tell two strangers on the same target apart. Every action on such a row is
  refused, the footer offers none of them, the refusal is on screen rather than
  silent, and Details credits the process to whoever started it instead of to gtunnel.
- **A mark and a header** - a cloud resting on a line, drawn in braille and banded in
  the four Google Cloud hues, with the platform beside the mark and the product beside
  the name. The art is one fixed size, because a braille glyph scaled up reads as a loose
  matrix of dots rather than a cloud. One module decides every colour the TUI uses, in
  named ANSI so it follows the terminal's own palette; the mark's hues are the documented
  exception.
- **Details** - beside the stored name, url and target, the pane says which GCP project
  the target is in, when the tunnel started, and whether anything brought it up without
  being asked - a launchd revival after a crash reads differently from a start.
- **List columns** - the tunnel list has a `NAME  MODE  URL` header and fixed columns,
  including one for the auto-start and health flags. It hides a name suffix equal to the
  tunnel's account; Details shows the full name, and `gtunnel list` is unchanged.
- **Resizing** - every pane follows the terminal. The name column is as wide as the
  longest name and shrinks only when the URL would otherwise be cut; below 121 columns
  the list stacks above the other panes instead of sharing a row with them; the list
  scrolls only when the selection leaves the window; long Details values wrap; and Logs
  keeps a minimum height, so it is still on screen at 24 rows.
- **Stacked from 121 columns down** - at 100 to 120 columns the list now takes the full
  width above the other panes, because its 46 percent share cut a 28-cell URL at 100 and
  110 columns and a 49-cell URL at 120.
- **Account field** - the header shows the active account and its GCP project id as a
  two-row filled field, so the project a key will act on is on screen. `;` switches both
  together. A long project id is cut with a `…` before the account name is ever shortened,
  and an account with no project shows the name alone.
- **Field colour** - the account field is filled with a shade of its own, one step lighter
  than the selected list row, so the header no longer reads as a second selection.
- **Keymap page and states** - `?` opens every bound key in four groups, two columns when
  the terminal is wide and one when it is not, with a note on the three case-sensitive
  pairs. A failed tunnel's footer leads with `R restart` in the warning colour, and an
  empty account says `no tunnels in this account` under the list header, and `loading`
  until its first load has answered, or `could not load tunnels` if that load failed. A
  page taller than the terminal scrolls with `j`/`k`, the arrows, `PgUp`/`PgDn` and
  `g`/`G`, and says where the reader is in it.
- **Dialogs that fit the terminal** - the add, edit, delete and mode dialogs and the
  instance and domain pickers are sized to their text and the terminal instead of a
  percentage of it, so a prompt wraps instead of being cut, the `Esc to cancel` row is on
  screen at 80x24, a long value keeps its end and the cursor in view, and a picker always
  shows the selected row.
- **Health notifications** - a tunnel going unhealthy raises a system notification.
- **`gtunnel demo`** - the whole dashboard filled with synthetic data, touching no GCP
  project and no config file.

### Operations

- **Ephemeral runs** - `gtunnel run` holds a tunnel in the foreground and tears down
  every route it created on exit, including on Ctrl+C and on a crash.
- **`gtunnel doctor`** - reports stale records, dead runs, orphaned relay resources,
  missing IAP firewall rules, broken credentials and a gcloud Python without NumPy.
  `--fix` removes only what is tagged `gtunnel:<account>` and unclaimed.
- **Ownership by tag, not by name** - every resource gtunnel creates carries
  `description: gtunnel:<account>`, so a hand-made resource whose name matches the
  scheme is never deleted.
- **Multiple accounts** - one gcloud configuration, project, zone and relay per account.
- **Self-update** - `gtunnel update` verifies the published SHA-256 before replacing the
  binary, and defers to Homebrew or cargo when that is how it was installed.

### Startup

- **Login check** - gtunnel probes the selected account's gcloud credentials before the
  TUI or any GCP command starts, runs `gcloud auth login` on a terminal when they are
  missing or expired, and exits 1 if the account is still not logged in afterwards.
  Without a terminal it exits 1 instead of prompting. `doctor`, `account`, `init`,
  `reset`, `demo` and `update` skip it.

### Platforms

- **macOS** - launchd user agents. Auto-start is the presence of the plist in
  `~/Library/LaunchAgents`; the definition gtunnel loads and unloads itself lives
  outside that directory, because launchd runs a `KeepAlive` job the moment it is
  loaded.
- **Linux** - systemd `--user` units. `loginctl enable-linger` is what makes a tunnel
  survive a logout.
- **Release binaries** - `macos-arm64`, `macos-intel`, `linux-amd64`, `linux-arm64`. The
  assets are named for the machine rather than the Rust target triple, so the file a
  visitor wants is the one they can recognise. The Linux pair are static musl builds.
