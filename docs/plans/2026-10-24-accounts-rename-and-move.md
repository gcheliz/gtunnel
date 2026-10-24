# Accounts, rename and move — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the TUI able to add a tunnel without dead ends, manage accounts, rename a tunnel
and move one between accounts, with every name and port collision caught before the dialog
closes rather than after.

**Architecture:** The collision rules already live in `state.rs` and `config.rs` and are correct;
what is missing is that the TUI never consults them until the operation is over. Three layers
change. `app.rs` gains validation at each dialog step and keeps the draft when an operation
fails. A new `src/commands/relocate.rs` holds rename and move, both of which are the same
mechanic — rewrite the service label and everything keyed to it — and both of which the CLI and
the TUI call. `account.rs` gains add, edit and a delete that refuses while tunnels remain.

**Tech Stack:** Rust, ratatui, tokio, launchd/systemd, anyhow.

**Spec:** the findings in this session's transcript; the four decisions recorded below.

## Decisions taken by the maintainer

| # | Decision |
|---|---|
| 1 | Deleting an account that still has tunnels **refuses** |
| 2 | Moving onto a name the destination already holds **offers a rename in the same dialog** |
| 3 | A **real rename** is built, not delete-plus-add |
| 4 | **One branch** for all of it |

## Global Constraints

- Tunnel names: `validate_tunnel_name` — up to 40 characters of `[a-z0-9-]`, first a lowercase
  letter, last a letter or digit. Account names: `validate_account_name`, up to 16.
- Names are unique per `(name, account)`, never globally. Local ports are machine-wide.
- A local port not given explicitly comes from `allocate_port(name, ACCESS_PORT_BASE,
  ACCESS_PORT_SPAN, &state.used_local_ports(), local_port_is_free)`. The CLI and the TUI must
  agree, because the README promises a name lands on the same port on every machine.
- The service label is `com.gtunnel.<account>.<name>` on macOS and the systemd unit equivalent
  elsewhere. Anything keyed to it moves with a rename: `agents/<label>.plist`, the
  `~/Library/LaunchAgents/<label>.plist` copy that *is* auto-start, and `logs/<account>-<name>.log`.
- A public tunnel additionally keys `hostname` = `<name>.<domain>`, the backend
  `gt-<account>-<name>` and the named port `gt-<name>` to its name.
- No emoji anywhere. Documentation uses placeholders only, per
  `.claude/rules/public-repo-placeholders.md`.
- Every task ends with `cargo fmt --check`, `cargo test`, and
  `cargo clippy --all-targets -- -D warnings` all clean.

## Scope boundary, stated once

**Move is Access-only in this branch.** A public tunnel's route lives in its account's project:
moving it means tearing down a backend, host rule, named port and DNS record in one project and
building them in another, against a relay that may not exist and a domain that may not be
registered there. `cmd_move` refuses an Expose tunnel with a message naming `delete` and
`expose` as the path. Rename, by contrast, covers both kinds, because `remove_route` and
`ensure_route` already exist and stay inside one project.

## Review Focus

1. A rename of a **running** tunnel must come back running, and one of a **stopped** tunnel must
   stay stopped — the label changes under a live process.
2. A rename of a tunnel with **auto-start on** must leave exactly one `~/Library/LaunchAgents`
   plist, under the new label.
3. A rename or move that **fails halfway** must leave the tunnel findable under one name, never
   two plists and no record, or a record whose label has no plist.
4. A move whose **destination account cannot see the instance** must refuse before writing
   anything — the zone in the spec belongs to the source account's project.
5. Removing the **last** account, and removing the **selected** account, must leave
   `selected_account` consistent with `accounts`.

---

### Task 1: The Add dialog validates the name where it is typed

**Files:**
- Modify: `src/tui/app.rs` (`InputMode::AddName` arm of `submit_line`, `open_add`, `cancel_dialog`)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: `crate::state::validate_tunnel_name`.
- Produces: `App::draft_error: Option<String>`, rendered under the prompt by Task 1's UI change.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_name_the_account_already_holds_is_refused_in_the_dialog() {
    // app_with_two() holds "db"; typing it must not advance past AddName
    // and must leave a message naming the clash
}

#[test]
fn an_invalid_name_is_refused_in_the_dialog() {
    // "Db!" keeps InputMode::AddName and reports the rule, not a parse error
}

#[test]
fn a_valid_new_name_advances_to_the_instance_step() {}
```

- [ ] **Step 2: Run them** — `cargo test a_name_the_account`. Expected: FAIL, the mode advances.
- [ ] **Step 3: Implement.** In the `AddName` arm, after the non-empty check, call
  `validate_tunnel_name` and then look for the name in `self.entries` restricted to
  `TunnelKind::Managed`. On either failure set `self.draft_error` and return without changing
  `input_mode`, keeping `self.input` so the typed text stays on screen.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Render `draft_error`** under the prompt in `src/tui/ui.rs`, in `theme::BAD`, and
  add a test that the message is on screen at 80x24.
- [ ] **Step 6: Commit** — `feat: the add dialog refuses a bad or taken name where it is typed`

---

### Task 2: A failed operation reopens the dialog instead of discarding the draft

**Files:**
- Modify: `src/tui/app.rs` (`cancel_dialog`, the `OpResult` handler around line 914)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: `OpResult { ok, message }`.
- Produces: `App::last_draft: Option<NewTunnel>` and the `InputMode` the draft had reached.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_create_that_failed_brings_the_draft_back() {
    // submit a draft, feed an OpResult { ok: false, .. },
    // assert the dialog is open again with name, instance and ports intact
}
```

- [ ] **Step 2: Run it** — Expected: FAIL, `input_mode` is `Normal` and the draft is default.
- [ ] **Step 3: Implement.** Keep the draft on submit rather than clearing it; on a failing
  `OpResult` for `Op::Create`, restore `self.draft` and set `input_mode` to the last step, with
  `draft_error` carrying the message.
- [ ] **Step 4: Run it** — Expected: PASS.
- [ ] **Step 5: Commit** — `feat: a create that failed reopens its dialog with the draft intact`

---

### Task 3: The instance step says when the list could not be read, and takes a typed name

**Files:**
- Modify: `src/tui/mod.rs:208-217` (the `want_instances` spawn), `src/tui/app.rs` (`pick_instance`)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Produces: `Msg::InstancesFailed(String)`, handled beside `Msg::Instances`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_instance_list_that_could_not_be_read_says_so() {
    // Msg::InstancesFailed leaves status_error true and the reason on screen
}

#[test]
fn an_instance_the_picker_never_offered_can_still_be_typed() {
    // with app.instances empty, typing "bastion" and pressing Enter sets
    // draft.instance to "bastion" and the account default zone, and advances
}
```

- [ ] **Step 2: Run them** — Expected: FAIL; today the error is dropped by `if let Ok(..)` and
  Enter answers `No instance matches that filter`.
- [ ] **Step 3: Implement.** Replace the `if let Ok` with a match that sends
  `Msg::InstancesFailed(format!("{e:#}"))`. In `pick_instance`, when the filter matches nothing
  and the typed text is non-empty, accept it as the instance with `self.default_zone`, so an
  account that cannot list instances is still usable.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Commit** — `feat: the instance step reports a failed list and accepts a typed name`

---

### Task 4: The local port the TUI suggests is the one the CLI would pick

**Files:**
- Modify: `src/tui/app.rs` (`suggested_local_port`), `src/tui/ops.rs` (`load_entries`),
  `src/tui/mod.rs` (the `Msg` carrying entries)
- Test: `src/tui/mod.rs`

**Interfaces:**
- Produces: `App::used_ports: HashSet<u16>`, filled from `TunnelState::used_local_ports()` by
  the same load that fills `entries`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_suggested_port_is_the_one_the_cli_would_allocate() {
    // with used_ports holding another account's port, the suggestion skips it
    // and equals allocate_port(name, ACCESS_PORT_BASE, ACCESS_PORT_SPAN, &used, |_| true)
}
```

- [ ] **Step 2: Run it** — Expected: FAIL; today the scan is `self.entries`, the selected
  account only, and starts at 15000 linearly rather than at the name hash.
- [ ] **Step 3: Implement** `suggested_local_port(&self, name: &str) -> u16` delegating to
  `allocate_port`, and carry `used_ports` through the load.
- [ ] **Step 4: Run it** — Expected: PASS.
- [ ] **Step 5: Commit** — `fix: the TUI suggests the port the CLI would allocate`

---

### Task 5: `a add` is on the footer whatever is selected

**Files:**
- Modify: `src/tui/ui.rs` (`ACTIONS_PROGRAM`, `ACTIONS_EMPTY`, `ACTIONS_UNMANAGED`)
- Test: `src/tui/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn adding_a_tunnel_is_on_the_footer_in_every_state() {
    // for each (status, kind) pair and the empty case, "a" is in one of the groups
}
```

- [ ] **Step 2: Run it** — Expected: FAIL for every selected row.
- [ ] **Step 3: Implement.** Move `Action::of("a")` to the head of `ACTIONS_PROGRAM`, which
  sheds from the front, and drop it from `ACTIONS_EMPTY` and `ACTIONS_UNMANAGED` so it is not
  listed twice. Check `footer_most()` still bounds the help page sentence.
- [ ] **Step 4: Run it** — Expected: PASS, and the existing shedding tests stay green.
- [ ] **Step 5: Commit** — `fix: the footer always offers a add`

---

### Task 6: Rename a tunnel

**Files:**
- Create: `src/commands/relocate.rs`
- Modify: `src/commands/mod.rs`, `src/cli.rs`
- Test: `src/commands/relocate.rs`

**Interfaces:**
- Produces: `pub async fn cmd_rename(account: Option<&str>, old: &str, new: &str) -> Result<()>`
- Consumes: `validate_tunnel_name`, `TunnelState::{find_for_account, add, remove_for_account}`,
  `daemon::{platform_label, status, stop, uninstall, install, service_spec, remove_log}`,
  `routes::{remove_route, ensure_route}`.

The order matters and is the whole task: read the tunnel and its running state, refuse early on
an invalid or taken new name, remove the old route if it is an Expose tunnel, stop and uninstall
the old label, rewrite the record (name, and for Expose the hostname), save, install under the
new label, restore auto-start, add the new route if it is an Expose tunnel, and start it again
only if it was running.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn renaming_to_a_name_the_account_holds_is_refused() {}
#[test] fn renaming_to_an_invalid_name_is_refused() {}
#[test] fn a_rename_moves_the_record_and_leaves_no_old_name() {}
#[test] fn renaming_an_expose_tunnel_rewrites_its_hostname() {}
```

- [ ] **Step 2: Run them** — Expected: FAIL, `cmd_rename` not defined.
- [ ] **Step 3: Implement** `cmd_rename` in the order above, with the state mutation split into
  a pure `rename_record(&mut PersistentTunnel, &str)` so the hostname rule is testable without
  a daemon.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Wire the CLI** — `gtunnel rename <old> <new>`, with `--account`.
- [ ] **Step 6: Run** `cargo test relocate` and the CLI parse tests. Expected: PASS.
- [ ] **Step 7: Commit** — `feat: gtunnel rename`

---

### Task 7: Move a tunnel between accounts

**Files:**
- Modify: `src/commands/relocate.rs`, `src/cli.rs`
- Test: `src/commands/relocate.rs`

**Interfaces:**
- Produces: `pub async fn cmd_move(name: &str, from: Option<&str>, to: &str, rename_to: Option<&str>) -> Result<()>`

`rename_to` is how decision 2 reaches the core: the TUI offers a rename when the destination
holds the name, and passes the chosen one here rather than aborting.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn moving_an_expose_tunnel_is_refused_with_the_path_to_take() {}
#[test] fn moving_onto_a_taken_name_without_a_rename_is_refused() {}
#[test] fn moving_onto_a_taken_name_with_a_rename_lands_under_the_new_name() {}
#[test] fn a_move_verifies_the_instance_exists_in_the_destination_first() {}
```

- [ ] **Step 2: Run them** — Expected: FAIL, `cmd_move` not defined.
- [ ] **Step 3: Implement.** Refuse `is_expose()`. Resolve both accounts. Verify the instance in
  the destination with `verify_instance` before any write. Check the destination name, applying
  `rename_to` when given. Then the same label dance as Task 6, with the account changing too.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Wire the CLI** — `gtunnel move <name> --to <account> [--rename <new>]`.
- [ ] **Step 6: Commit** — `feat: gtunnel move, for access tunnels`

---

### Task 8: Account add, edit, and a delete that refuses

**Files:**
- Modify: `src/commands/account.rs`, `src/cli.rs`
- Test: `src/commands/account.rs`

**Interfaces:**
- Produces: `cmd_account_add(configuration: &str, name: &str) -> Result<()>`,
  `cmd_account_edit(name: &str, project: Option<&str>, zone: Option<&str>) -> Result<()>`,
  and the changed `cmd_account_remove`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn removing_an_account_that_still_has_tunnels_is_refused() {}
#[test] fn removing_the_selected_account_moves_the_selection() {}
#[test] fn editing_an_account_changes_only_the_fields_given() {}
```

- [ ] **Step 2: Run them** — Expected: FAIL; today remove prints a warning and proceeds.
- [ ] **Step 3: Implement.** `cmd_account_remove` bails naming the count and the two ways out:
  delete them, or move them. `cmd_account_add` takes a gcloud configuration non-interactively,
  which is what the TUI page needs. `cmd_account_edit` writes project and zone.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Wire the CLI** — `account add`, `account edit`.
- [ ] **Step 6: Commit** — `feat: account add and edit, and a remove that refuses`

---

### Task 9: The accounts page in the TUI

**Files:**
- Modify: `src/tui/app.rs`, `src/tui/ui.rs`, `src/tui/ops.rs`, `src/tui/mod.rs`
- Test: `src/tui/mod.rs`

**Interfaces:**
- Produces: `InputMode::Accounts` and the ops `Op::AccountAdd`, `Op::AccountEdit`,
  `Op::AccountRemove`, `Op::AccountSelect`.
- The key is `:` — the shift of the `;` that cycles accounts, and not a letter, so it joins
  none of the three case-sensitive pairs the keymap page warns about.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn the_accounts_page_lists_every_account_and_marks_the_selected_one() {}
#[test] fn deleting_an_account_with_tunnels_says_why_it_cannot() {}
#[test] fn the_accounts_page_is_in_the_keymap_and_on_the_footer() {}
```

- [ ] **Step 2: Run them** — Expected: FAIL.
- [ ] **Step 3: Implement** the page and its four ops, reusing the existing dialog sizing so it
  fits 80x24, and add `:` to `KEYMAP` under `Group::Program`.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Commit** — `feat: an accounts page that creates, edits, selects and removes`

---

### Task 10: Rename and move from the TUI, behind the edit key

**Files:**
- Modify: `src/tui/app.rs`, `src/tui/ui.rs`, `src/tui/ops.rs`, `src/tui/mod.rs`
- Test: `src/tui/mod.rs`

**Interfaces:**
- Consumes: `cmd_rename`, `cmd_move` from Task 6 and Task 7.
- Produces: `InputMode::ChooseEdit`, `InputMode::RenameTo`, `InputMode::MoveTo`,
  `InputMode::MoveRename`, and `Op::Rename`, `Op::Move`.

`e` stops opening `EditTarget` directly and opens a three-way chooser instead — `1` target and
ports, `2` rename, `3` move to another account — the same shape as the existing `ChooseMode`
dialog, so no new top-level key is spent and no new case pair is created.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn e_offers_target_rename_and_move() {}
#[test] fn renaming_from_the_tui_refuses_a_taken_name_in_the_dialog() {}
#[test] fn moving_onto_a_taken_name_offers_a_rename_rather_than_cancelling() {}
#[test] fn moving_an_unmanaged_or_ephemeral_row_is_refused() {}
```

- [ ] **Step 2: Run them** — Expected: FAIL.
- [ ] **Step 3: Implement.** The collision check in `MoveTo` reads the destination account's
  names and, on a clash, advances to `MoveRename` with the reason on screen rather than
  cancelling — decision 2.
- [ ] **Step 4: Run them** — Expected: PASS.
- [ ] **Step 5: Commit** — `feat: rename and move a tunnel from the TUI`

---

### Task 11: Documentation

**Files:**
- Modify: `README.md`, `CHANGELOG.md`, `docs/usage.md`

- [ ] **Step 1:** `docs/usage.md` gains `rename`, `move`, `account add` and `account edit`, each
  with captured output from a throwaway `GTUNNEL_CONFIG_DIR`, placeholders only.
- [ ] **Step 2:** README's CLI table gains the two commands; the TUI section gains `:` and the
  `e` chooser; the keymap count line is checked against `keys_bound()`.
- [ ] **Step 3:** CHANGELOG gains one entry per decision, saying what is refused and why.
- [ ] **Step 4:** Run the placeholder sweep from `.claude/rules/public-repo-placeholders.md`.
  Expected: no output.
- [ ] **Step 5:** Re-record `docs/demo-access.gif` if the add flow's prompts changed on screen.
- [ ] **Step 6: Commit** — `docs: rename, move and account management`

---

## Self-review

- **Spec coverage:** all four decisions have a task — refuse (8), rename offer (7, 10), real
  rename (6), one branch (this file). All six findings have a task: footer (5), instance dead
  end (3), dest-group is explicitly **not** in scope and stays an open item, port suggestion (4),
  late validation (1, 2), account CRUD (8, 9), move (7, 10).
- **Type consistency:** `cmd_move` takes `rename_to: Option<&str>` and Task 10 is the only
  caller that passes `Some`. `suggested_local_port` gains a `name` parameter in Task 4; its one
  caller is the `AddLocalPort` prompt.
- **Known gap, deliberate:** the TUI still cannot create an IAP destination-group tunnel. It is
  not in this branch and stays on the open-items list.
