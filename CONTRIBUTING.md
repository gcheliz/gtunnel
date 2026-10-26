# Contributing

Thanks for looking. gtunnel is a single Rust binary with no runtime services of its own, so
getting from a clone to a green test run takes one command.

## Getting set up

| Requirement | Why | Check |
|---|---|---|
| Rust 1.88 or newer | the floor `ratatui` 0.30 sets, declared in `Cargo.toml` | `cargo --version` |
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

CI runs six checks on every pull request. Run them before you push and you will not be
surprised:

| Command | What fails it |
|---|---|
| `cargo fmt --check` | any formatting drift |
| `cargo clippy --all-targets -- -D warnings` | every clippy warning, including the lints declared in `Cargo.toml` |
| `cargo test` | the suite, on ubuntu and macos separately |
| `cargo audit --deny warnings` | a dependency with an advisory |
| `cargo check --locked --all-targets` on 1.88 | anything that needs a newer compiler than declared |
| `zizmor --offline .github/workflows` | an unpinned action or a template injection |

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

## Documentation is part of the change

A change that alters behaviour updates the docs in the same pull request.

| Change | Where it is documented |
|---|---|
| a command, a flag, or its output | [docs/usage.md](docs/usage.md) |
| topology, process model, resource names | [docs/architecture.md](docs/architecture.md) |
| a new error or failure mode | [docs/troubleshooting.md](docs/troubleshooting.md) |
| how the TUI presents something | [docs/design.md](docs/design.md) |
| a choice someone would otherwise re-litigate | [docs/decisions.md](docs/decisions.md) |
| anything a user would notice | `CHANGELOG.md` |

One topic, one file. If a doc for your topic exists, add a section to it rather than
starting a second file.

### Placeholders, always

This repository is public, and a set of individually harmless identifiers is an inventory
of somebody's network. Nothing in `README.md`, `docs/`, doc comments, help text or test
fixtures may carry a real value.

| Kind | Write |
|---|---|
| GCP project | `example-dev-1a2b`, `example-prod-3c4d` |
| Account | `you@example.com` |
| Instance | `bastion`, `my-vm` |
| Private address | `10.0.0.10` |
| Internal hostname | `db.internal` |
| Username in captured output | `you` |

Zones and regions such as `europe-west1-b` stay real: they are public GCP identifiers and a
reader needs a valid one to copy. A test fixture counts as an example, because that is how
it reads.

To capture real command output for a doc without leaking anything, point gtunnel at a
throwaway configuration:

```
GTUNNEL_CONFIG_DIR=/tmp/gtunnel-demo cargo run -- add k8s 10.0.0.10:443 --via bastion
```

That moves `config.toml`, `tunnels.toml`, `agents/` and `logs/` into that directory. Give
the throwaway account a name of its own: the service label is
`com.gtunnel.<account>.<name>`, so reusing a live account's name restarts its job.

Then substitute only the value under discussion. A transcript edited further is no longer
evidence of anything.

## Commits and pull requests

Commit subjects use a conventional prefix and the imperative mood:

```
feat: name release assets for the machine, not the target triple
fix: stop the relay probe from counting a closed port as healthy
docs: the relay port range is open until you close it
```

`feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `ci`. The prefix is load-bearing: it
drives the version bump and the changelog entry.

The body explains the constraint that made the change necessary. The reviewer reads the
diff; the message explains it.

A pull request description says what changed, why, and what the impact is. Omit a section
that has nothing to say rather than filling it, and skip the commit list — the Commits tab
already has it and every SHA rots on the next rebase.

`main` takes squash merges only, so your branch's working commits never reach the published
history. Write the pull request title as the commit message you want on `main`.

## Releases

You do not need to think about versions when you contribute. Use the right commit prefix
and the rest follows.

Every merge to `main` is released by [`release.yml`](.github/workflows/release.yml) in
one run. [`release-plz`](https://release-plz.dev) reads the conventional-commit subjects
since the last tag and works out the next version; the workflow tests the merge, opens a
draft release, commits the bump (`Cargo.toml`, `Cargo.lock`, a `CHANGELOG.md` entry) and
pushes it with the tag. There is no release pull request to merge.

gtunnel is below 1.0, and semver treats a `0.x` version as unstable throughout, so every
bump is smaller than it would be after 1.0. The right-hand column is what happens today,
not after the first stable release.

| Prefix | Version | Changelog |
|---|---|---|
| `feat` | patch, because a feature is not breaking and `0.x` has no minor to spend on it | Added |
| `fix` | patch | Fixed |
| `perf` | patch | Performance |
| `refactor` | patch | Changed |
| `feat!`, or a `BREAKING CHANGE:` trailer | minor; it becomes major at 1.0 | under its own prefix |
| `docs` | patch | Documentation |
| `test`, `ci`, `chore`, `build` | none | none |

Every row that writes a changelog line releases on merge, with nothing to dispatch. The
last row writes none and changes nothing a user gets, so it releases nothing; its commits
reach users with the next release.

**The pull request title is the commit message.** `main` takes squash merges, so the title
is the only subject that reaches the history and the only one release-plz reads. A branch
whose commits are a `feat` and two `docs` merged under a `docs:` title ships the feature
with no Added line and no sign of it in the release. Title the pull request after the most
significant change in it.

The tag push starts `release.yml` again on the tag: it builds the four targets, writes a
`.sha256` beside each tarball, attests every asset, and publishes the draft as the latest
release. See [decision 14](docs/decisions.md).

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
