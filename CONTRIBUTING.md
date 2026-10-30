# Contributing

Thanks for looking. gtunnel is a single Rust binary with no runtime services of its own, so
getting from a clone to a green test run takes one command.

## Getting set up

| Requirement | Why | Check |
|---|---|---|
| Rust 1.89 or newer | the floor `std::fs::File::lock` sets, declared in `Cargo.toml` | `cargo --version` |
| Google Cloud SDK | only to run gtunnel against a real project; not needed to build or test | `gcloud version` |

```
git clone https://github.com/gcheliz/gtunnel
cd gtunnel
cargo test
```

The test suite needs no GCP project, no network and no credentials. If it needs any of
those to pass on your machine, that is a bug worth reporting.

To see the interface without touching a cloud project:

```
cargo run -- demo
```

`demo` fills the TUI with synthetic data and reads no config file.

## The gate

Every pull request runs these checks; `main` requires all of them. Run the commands before
you push and you will not be surprised:

| Command | What fails it |
|---|---|
| `cargo fmt --check` | any formatting drift |
| `cargo clippy --all-targets -- -D warnings` | every clippy warning, including the lints declared in `Cargo.toml` |
| `cargo test` | the suite, on ubuntu and macos separately |
| `cargo deny --locked check` | a dependency with an advisory, a licence outside `deny.toml`, or a crate from an unknown source |
| `cargo check --locked --all-targets` on 1.89 | anything that needs a newer compiler than declared |
| `zizmor --offline .github/workflows` | an unpinned action or a template injection |
| Verify documentation versions (Linux test job) | an install snippet, `--version` sample or landing-page marker that does not quote the version in `Cargo.toml` |
| `pr-title` | a pull request title that is not a conventional commit subject, since the squash uses it as the message on `main` |

All checks must be green and the branch must be up to date with `main` before a
pull request can merge. Enable auto-merge and GitHub will do it for you when they pass.

`rustfmt.toml` pins `style_edition = "2021"` on purpose: the 2024 style would reflow two
dozen files, and that belongs in a formatting-only change of its own, not riding along with
your fix.

## Writing the change

**Tests come first.** Write the failing test, watch it fail for the reason you expect, then
make it pass. A test that passed the first time you ran it has not been shown to catch
anything.

**Keep the diff about one thing.** A rename, a reformat and a behaviour change in one pull
request cannot be reviewed; they can only be approved.

**Say why, not what, in comments.** The code says what it does. A comment earns its place
by recording the constraint that made the code look like that.

**`unsafe` carries its invariant.** `undocumented_unsafe_blocks` is a lint here, so every
`unsafe` block needs a `SAFETY:` comment stating what makes it sound. `dbg!`, `todo!` and
`unimplemented!` are lints too and will not reach a release binary.

## Tests: four layers

Pick the lowest layer that can fail for the reason you care about. A higher layer costs
more to run and says less about where it broke.

| Layer | Lives in | Fake | Use it for |
|---|---|---|---|
| Unit | `#[cfg(test)]` modules in `src/` | none; `Endpoints::single` and a static token for a client | parsing, formatting, a decision made from values |
| Component | `src/gcp/*` tests | a wiremock `MockServer` behind `GcpClient::new(TokenSource::Static, Endpoints::single(..))` | one API call: its URL, body and error handling |
| Binary integration | `tests/*.rs` | `Harness`: a stub `gcloud` on `PATH`, a scratch config, and for GCP a wiremock server named by `GTUNNEL_TEST_GCP_ENDPOINT` | a command as a person types it: exit code, output and saved files together |
| End to end | not yet | a real project | nothing runs here today |

### Using `Harness`

`tests/common/mod.rs` holds the one harness. A test file starts with `mod common;`.

```rust
mod common;
use common::{text, Harness, CONFIG, LOGGED_IN_STUB};

#[tokio::test(flavor = "multi_thread")]
async fn add_saves_the_zone_the_api_reports() {
    let server = wiremock::MockServer::start().await;
    // mount the compute responses the command asks for
    let h = Harness::build(&[("gcloud", LOGGED_IN_STUB)], CONFIG, None).with_gcp(&server);
    let out = h.run(&["add", "pg", "10.0.0.10:5432", "--via", "bastion"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
}
```

| Piece | Does |
|---|---|
| `Harness::with_stub(stub)` | a `gcloud` stub, the `demo` account and one saved tunnel |
| `Harness::build(stubs, config, tunnels)` | the same with your own stubs, config and tunnels (`None` writes no `tunnels.toml`) |
| `.with_env(key, value)`, `.with_inherited_path()` | one more variable for every run; the caller's `PATH` after the stubs |
| `.with_gcp(&server)` | every GCP API of the binary answers from that wiremock server |
| `run(args)`, `run_with_env(args, envs)` | run to completion and return the `Output` |
| `spawn(args, envs)`, `wait_exit(n, bound)` | start a run that stays; `Drop` ends what a failed test leaves |
| `poll(bound, condition)` | wait on a condition with a bound; never a bare `sleep` |

Two rules for a GCP test. Use an account name no other test uses, because a launchd label is
`com.gtunnel.<account>.<name>` and a test must not share one with a real config. And never
start a tunnel (`-s`, `start`): the job would outlive the test.

`GTUNNEL_TEST_GCP_ENDPOINT` exists only in a debug build, which is what `cargo test` runs.
A release build does not contain the name, and CI builds `--release` and fails if it finds it.

### A TUI change: PTY test or render test

Two kinds of test cover the TUI. Pick by what the change can break.

| The change touches | Test | Where |
|---|---|---|
| A key reaching the app, or what it opens | PTY | `tests/tui_pty.rs` |
| Terminal setup and restore: alternate screen, raw mode, cursor | PTY | `tests/tui_pty.rs` |
| Resize, or anything that must survive one | PTY | `tests/tui_pty.rs` |
| The real binary's wiring: `main`, dispatch, the event loop | PTY | `tests/tui_pty.rs` |
| Layout, truncation, colour, what a pane says | `TestBackend` render | `src/tui/ui/tests.rs` |

A PTY test runs `gtunnel demo` in a 100x30 terminal, types keys and reads the screen back,
so it is slow and says nothing about layout. A render test draws `App` into a buffer and is
fast, but cannot see a key that never arrives. If the change is a layout, write the render
test; if it is a key or the terminal, write the PTY test; a new key that also changes a
layout needs both. A PTY test waits on the screen with a bound, never a fixed sleep.

### Doc transcripts

`tests/doc_transcripts.rs` runs the blocks fenced `console` in `docs/commands.md`,
`docs/usage.md` and `docs/configuration.md` against the binary (macOS only, with a stub
`launchctl` first on `PATH`). A transcript that needs a live tunnel, a prompt or real GCP is
fenced `console,ignore`; a block that is not a transcript is `text`, never a bare fence. A
block ends with one blank line, a `? N` line after the command is a non-zero exit code, and
`[HOME]` is the scratch home. When a message changes, `TRYCMD=overwrite cargo test --test
doc_transcripts` rewrites the blocks: read the diff first, because what it writes is what the
binary printed.

## Documentation is part of the change

A change that alters behaviour updates the docs in the same pull request.

| Change | Where it is documented |
|---|---|
| a command, a flag, or its output | [docs/commands.md](docs/commands.md) |
| a config key, a file gtunnel writes, or an environment variable | [docs/configuration.md](docs/configuration.md) |
| a mode, a target kind, setup, the TUI or the MCP server | [docs/usage.md](docs/usage.md) |
| topology, process model, resource names | [docs/architecture.md](docs/architecture.md) |
| a new error or failure mode | [docs/troubleshooting.md](docs/troubleshooting.md) |
| how the TUI presents something | [docs/design.md](docs/design.md) |
| a choice someone would otherwise re-litigate | [docs/decisions.md](docs/decisions.md) |
| anything a user would notice | `CHANGELOG.md` |

One topic, one file. If a doc for your topic exists, add a section to it rather than
starting a second file.

The recordings in `docs/` are generated from the [VHS](https://github.com/charmbracelet/vhs)
tapes in [docs/tapes](docs/tapes) with `vhs docs/tapes/<name>.tape`.

### Placeholders, always

Nothing in `README.md`, `docs/`, doc comments, help text or test fixtures may carry a real
value; a fixture counts, because it reads as an example. Why:
[decision 7](docs/decisions.md#7-documentation-uses-placeholders-never-real-configuration).

| Kind | Write |
|---|---|
| GCP project | `example-dev-1a2b`, `example-prod-3c4d` |
| Account | `you@example.com` |
| Instance | `bastion` |
| Private address | `10.0.0.10` |
| Internal hostname | `db.internal` |
| Username in captured output | `you` |

Zones and regions such as `europe-west1-b` stay real. Capture output against a throwaway
configuration:

```
GTUNNEL_CONFIG_DIR=/tmp/gtunnel-demo cargo run -- add k8s 10.0.0.10:443 --via bastion
```

Give the throwaway account a name of its own: the service label is
`com.gtunnel.<account>.<name>`, so a live account's name restarts its job. Then substitute only
the value under discussion.

## Issues

The tracker is where a defect or a planned change lives; a plan file or a chat message is
not a record. Search before you open one (`gh issue list --state all --search "<keywords>"`)
and extend a match rather than duplicating it. A pull request says `Closes #N`, and the
merge closes the issue, never a hand while the fix is unmerged.

| Section | Content |
|---|---|
| Symptom | What a person using gtunnel sees, in one or two sentences |
| Where | `file:line` at a named commit, so the line survives drift |
| Why | The mechanism, not the history of how it was found |
| Fix shape | The design already agreed, so the implementer does not re-decide it |
| Acceptance | The test or command that proves it, as a checklist |

## Commits and pull requests

Commit subjects use a conventional prefix and the imperative mood:

```
feat: name release assets for the machine, not the target triple
fix: stop the relay probe from counting a closed port as healthy
docs: the relay port range is open until you close it
```

`feat`, `fix`, `perf`, `refactor`, `docs`, `build`, `test`, `chore`, `ci`. The prefix is
load-bearing: it drives the version bump and the changelog entry.

The body explains the constraint that made the change necessary. The reviewer reads the
diff; the message explains it.

A pull request description says what changed, why, and what the impact is. Omit a section
that has nothing to say rather than filling it, and skip the commit list — the Commits tab
already has it and every SHA rots on the next rebase.

`main` takes squash merges only, so your branch's working commits never reach the published
history. Write the pull request title as the commit message you want on `main`.

## Releases

You do not pick versions. Every merge to `main` runs [`release.yml`](.github/workflows/release.yml):
[`release-plz`](https://release-plz.dev) reads the commit subjects since the last tag,
computes the version, and the workflow tests the merge, commits the bump (`Cargo.toml`,
`Cargo.lock`, a `CHANGELOG.md` entry), tags it, builds and publishes. There is no release pull
request. Why it works this way: [decision 14](docs/decisions.md#14-releases-are-computed-and-published-automatically).

Every GitHub API call in the train goes through [`gh-retry.sh`](.github/scripts/gh-retry.sh), which
reruns it up to three times, after 5s and 15s, only on an HTTP 5xx, a reset connection or a
timeout; a 4xx fails at once.

Below 1.0 every bump is one step smaller than semver after 1.0:

| Prefix | Version | Changelog |
|---|---|---|
| `feat` | patch (`0.x` has no minor to spend on a non-breaking change) | Added |
| `fix` | patch | Fixed |
| `perf` | patch | Performance |
| `refactor` | patch | Changed |
| `feat!`, or a `BREAKING CHANGE:` trailer | minor; major at 1.0 | under its own prefix |
| `docs` | none on its own: the line ships with the next release | Documentation |
| `test`, `ci`, `chore`, `build` | none | none |
| any prefix, changing only `Cargo.lock` | patch | Dependencies |

A lockfile-only merge (a Dependabot cargo update) releases only while it is the sole change
since the last tag; a second merge on top hides it until the next release.

**The pull request title is the commit message.** The squash keeps only the title, and
release-plz reads only that. A `feat` merged under a `docs:` title ships with no Added line
and no release. Title the pull request after its most significant change.

A maintainer can rehearse the train without releasing anything:

```
gh workflow run release.yml --ref <branch> -f dry_run=true
```

A dry run computes the version, runs CI, opens a draft named `vX.Y.Z-dry-run` and builds,
but pushes no commit or tag and publishes nothing. Delete that `-dry-run` draft afterwards;
the draft without the suffix belongs to a real train.

## Reporting something

| What | Where |
|---|---|
| A bug or a feature request | [Issues](https://github.com/gcheliz/gtunnel/issues) |
| A security vulnerability | privately, per [SECURITY.md](SECURITY.md) — never in a public issue |

A bug report is most useful with the command you ran, what you expected, what happened, and
the output of `gtunnel doctor`. Check that `doctor`'s output carries no real project id,
host or address before you paste it.

## License

By contributing you agree that your work is licensed under the MIT license in
[LICENSE](LICENSE).
