// `gtunnel run` driven through the real binary with a stub `gcloud` that, like
// the real one, is a launcher: it starts a child of its own and waits for it.
// What these prove is what a person would see with `ps` after the run ends:
// neither the stub nor its child is left, however the run was stopped.
//
// Every wait is a bounded poll on the condition; a sleep never stands in for
// one, so a slow machine costs time and not a flake.

mod common;

use common::{poll, Harness, BOUND};
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

// The stub's child is written next to its own pid so a test can look for both,
// after the first line and not before: a run killed between the two would
// leave that line to a pipe with no reader, and SIGPIPE would end the stub.
// STUB_STUBBORN gives the child a SIGTERM handler that ignores the signal, as
// an ssh holding a forward may; STUB_EXIT_AFTER makes the stub itself exit
// first and leave that child behind, with STUB_EXIT_CODE as its code.
// STUB_CLOSE_STDOUT prints one byte that is not UTF-8 and closes the stub's
// stdout while it stays up, which its child does not hold open.
const STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token") echo stub-token; exit 0 ;;
esac
[ "$1" = compute ] || exit 0
if [ -n "$STUB_STUBBORN" ]; then (trap '' TERM; exec sleep 300) &
elif [ -n "$STUB_CLOSE_STDOUT" ]; then sleep 300 >/dev/null &
else sleep 300 & fi
echo "Listening on port [15000]."
echo "$$ $!" >> "$STUB_DIR/pids"
if [ -n "$STUB_CLOSE_STDOUT" ]; then printf '\377\n'; exec >&-; fi
if [ -n "$STUB_EXIT_AFTER" ]; then sleep "$STUB_EXIT_AFTER"; exit "${STUB_EXIT_CODE:-1}"; fi
wait
"#;

const CONFIG: &str = r#"selected_account = "dev"

[[accounts]]
name = "dev"
gcloud_configuration = "work"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"
"#;

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; the pid was read from the stub's own
    // file and is positive.
    unsafe { libc::kill(pid, 0) == 0 }
}

impl Harness {
    // No tunnels.toml: these runs are ephemeral and save nothing
    fn new() -> Harness {
        Harness::build(&[("gcloud", STUB)], CONFIG, None)
    }

    /// Run to completion, bounded; for the runs that are refused and so end at
    /// once. A run that wrongly starts is left to `Drop` and reads as no exit
    /// code, so the test fails instead of hanging.
    fn run_bounded(&self, args: &[&str]) -> (Option<i32>, String) {
        let n = self.spawn(args, &[]);
        let code = self.wait_exit(n, BOUND).and_then(|s| s.code());
        (code, self.stderr(n))
    }

    fn terminate(&self, n: usize) {
        // SAFETY: SIGTERM to a gtunnel this harness spawned and still holds.
        unsafe { libc::kill(self.pid(n), libc::SIGTERM) };
    }

    /// (stub, its child) for every gcloud the stub has started.
    fn pids(&self) -> Vec<(i32, i32)> {
        fs::read_to_string(self.root().join("stub/pids"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let (a, b) = l.split_once(' ')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            })
            .collect()
    }

    fn records(&self) -> Vec<serde_json::Value> {
        let Ok(dir) = fs::read_dir(self.root().join("config/run")) else {
            return Vec::new();
        };
        let mut paths: Vec<PathBuf> = dir.filter_map(|e| Some(e.ok()?.path())).collect();
        paths.sort();
        paths
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .filter_map(|p| serde_json::from_str(&fs::read_to_string(p).ok()?).ok())
            .collect()
    }

    fn wait_for_records(&self, count: usize) -> bool {
        poll(BOUND, || self.records().len() == count)
    }

    /// Both stub processes gone and the record removed, each within 5 s.
    fn assert_nothing_left(&self) {
        let pids = self.pids();
        assert!(!pids.is_empty(), "the stub never started");
        for (gcloud, child) in pids {
            assert!(
                poll(Duration::from_secs(5), || !alive(gcloud)),
                "gcloud {gcloud} survived"
            );
            assert!(
                poll(Duration::from_secs(5), || !alive(child)),
                "gcloud's child {child} survived"
            );
        }
        assert!(
            poll(Duration::from_secs(5), || self.records().is_empty()),
            "a record was left: {:?}",
            self.records()
        );
    }
}

#[test]
fn a_run_stopped_by_its_ttl_leaves_neither_gcloud_nor_its_child() {
    let h = Harness::new();
    let n = h.spawn(&["run", "--ttl", "2s", "db", "my-vm:5432"], &[]);
    let status = h.wait_exit(n, BOUND).expect("the run ended by itself");
    assert_eq!(status.code(), Some(0), "stdout: {}", h.stdout(n));
    assert!(
        h.stdout(n).contains("TTL reached, stopping"),
        "stdout: {}",
        h.stdout(n)
    );
    h.assert_nothing_left();
}

#[test]
fn a_run_stopped_by_sigterm_leaves_neither_gcloud_nor_its_child() {
    let h = Harness::new();
    let n = h.spawn(&["run", "db", "my-vm:5432"], &[]);
    assert!(h.wait_for_records(1), "the run never published a record");
    // The record is saved at spawn, before the stub has started its child
    assert!(
        poll(BOUND, || !h.pids().is_empty()),
        "the stub never started"
    );
    h.terminate(n);
    assert!(h.wait_exit(n, BOUND).is_some(), "the run ignored SIGTERM");
    h.assert_nothing_left();
}

#[test]
fn a_child_that_ignores_sigterm_is_gone_after_the_grace() {
    let h = Harness::new();
    let n = h.spawn(
        &["run", "--ttl", "2s", "db", "my-vm:5432"],
        &[("STUB_STUBBORN", "1")],
    );
    assert!(h.wait_exit(n, BOUND).is_some(), "the run never ended");
    h.assert_nothing_left();
}

#[test]
fn a_gcloud_that_exits_by_itself_leaves_no_child() {
    let h = Harness::new();
    let n = h.spawn(
        &["run", "db", "my-vm:5432"],
        &[("STUB_STUBBORN", "1"), ("STUB_EXIT_AFTER", "1")],
    );
    assert!(h.wait_exit(n, BOUND).is_some(), "the run never ended");
    assert!(
        h.stdout(n).contains("gcloud exited with"),
        "stdout: {}",
        h.stdout(n)
    );
    h.assert_nothing_left();
}

#[test]
fn a_second_run_with_a_live_name_is_refused_and_the_first_record_is_untouched() {
    let h = Harness::new();
    let first = h.spawn(&["run", "db", "my-vm:5432"], &[]);
    assert!(
        h.wait_for_records(1),
        "the first run never published a record"
    );
    assert!(
        poll(BOUND, || !h.pids().is_empty()),
        "the stub never started"
    );
    let before = h.records();

    let (code, err) = h.run_bounded(&["run", "db", "my-vm:5432"]);
    assert_eq!(code, Some(2), "stderr: {err}");
    assert!(
        err.contains("A run named 'db' is already live for account 'dev'"),
        "stderr: {err}"
    );
    assert_eq!(h.pids().len(), 1, "the refused run started no gcloud");
    let after = h.records();
    assert_eq!(after.len(), 1);
    assert_eq!(before[0]["pid"], after[0]["pid"]);
    assert_eq!(before[0]["pid_start"], after[0]["pid_start"]);
    assert!(
        h.wait_exit(first, Duration::from_millis(1)).is_none(),
        "the first run is still running"
    );
}

#[test]
fn run_refuses_local_port_zero() {
    let h = Harness::new();
    let (code, err) = h.run_bounded(&["run", "--local-port", "0", "db", "my-vm:5432"]);
    assert_eq!(code, Some(2), "stderr: {err}");
    assert!(
        err.contains("Local port must be between 1 and 65535"),
        "stderr: {err}"
    );
    assert!(h.pids().is_empty(), "no gcloud for a refused port");
}

impl Harness {
    /// Like `spawn`, but stdout is a pipe whose reader is already gone, which
    /// is what `gtunnel run ... | head -1` leaves behind once head has exited.
    fn spawn_with_closed_stdout(&self, args: &[&str]) -> usize {
        let n = self.next_index();
        let err = fs::File::create(self.root().join(format!("err-{n}"))).unwrap();
        let (reader, writer) = std::io::pipe().expect("pipe");
        drop(reader);
        let child = self
            .command(args, &[])
            .stdout(Stdio::from(writer))
            .stderr(err)
            .spawn()
            .expect("spawn gtunnel");
        self.adopt(child)
    }

    fn kill_hard(&self, n: usize) {
        // SAFETY: SIGKILL to a gtunnel this harness spawned and still holds.
        unsafe { libc::kill(self.pid(n), libc::SIGKILL) };
        assert!(
            self.wait_exit(n, BOUND).is_some(),
            "the run survived SIGKILL"
        );
    }

    fn record_path(&self) -> PathBuf {
        fs::read_dir(self.root().join("config/run"))
            .unwrap()
            .filter_map(|e| Some(e.ok()?.path()))
            .find(|p| p.extension().is_some_and(|e| e == "json"))
            .expect("a run record")
    }

    fn wait_for_stub(&self) {
        assert!(self.wait_for_records(1), "the run never published a record");
        assert!(
            poll(BOUND, || !self.pids().is_empty()),
            "the stub never started"
        );
    }
}

#[test]
fn a_run_whose_output_reader_has_gone_keeps_the_tunnel_until_its_ttl() {
    let h = Harness::new();
    let n = h.spawn_with_closed_stdout(&["run", "--ttl", "4s", "db", "my-vm:5432"]);
    h.wait_for_stub();
    // The stub's first line has been (or is being) written to a closed pipe: a
    // run that panicked on it would be gone by now, taking its TTL with it
    assert!(
        h.wait_exit(n, Duration::from_millis(1500)).is_none(),
        "the run ended when its stdout closed: {}",
        fs::read_to_string(h.root().join(format!("err-{n}"))).unwrap_or_default()
    );
    let status = h.wait_exit(n, BOUND).expect("the TTL ended the run");
    assert_eq!(status.code(), Some(0));
    h.assert_nothing_left();
}

#[test]
fn a_run_whose_output_reader_has_gone_still_stops_on_sigterm() {
    let h = Harness::new();
    let n = h.spawn_with_closed_stdout(&["run", "db", "my-vm:5432"]);
    h.wait_for_stub();
    assert!(h.wait_exit(n, Duration::from_millis(1000)).is_none());
    h.terminate(n);
    assert!(h.wait_exit(n, BOUND).is_some(), "the run ignored SIGTERM");
    h.assert_nothing_left();
}

#[test]
fn a_run_killed_outright_is_stopped_and_forgotten_by_the_next_list() {
    let h = Harness::new();
    let n = h.spawn(&["run", "db", "my-vm:5432"], &[]);
    h.wait_for_stub();
    let record = h.records().remove(0);
    assert_eq!(record["owner_pid"], h.pid(n), "the record names its owner");

    h.kill_hard(n);
    // Nothing ran at exit, so gcloud is still up and the record is still there.
    // Linux ends gcloud with its owner (PR_SET_PDEATHSIG) and leaves its child
    #[cfg(not(target_os = "linux"))]
    assert!(alive(h.pids()[0].0), "gcloud outlives its owner");

    let (code, err) = h.run_bounded(&["list"]);
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(err.contains("Stopped run db"), "stderr: {err}");
    h.assert_nothing_left();
}

#[test]
fn a_run_past_its_ttl_with_no_owner_on_record_is_stopped_by_the_next_list() {
    let h = Harness::new();
    let n = h.spawn(&["run", "db", "my-vm:5432"], &[]);
    h.wait_for_stub();
    h.kill_hard(n);
    // As a record from before the owner fields would look, a day old
    let path = h.record_path();
    let mut record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let fields = record.as_object_mut().unwrap();
    fields.remove("owner_pid");
    fields.remove("owner_start");
    fields.insert("ttl_secs".into(), 60.into());
    fields.insert("started_at".into(), 1_700_000_000u64.into());
    fs::write(&path, record.to_string()).unwrap();

    let (code, err) = h.run_bounded(&["list", "--json"]);
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(err.contains("past its TTL"), "stderr: {err}");
    h.assert_nothing_left();
}

#[test]
fn a_non_utf8_byte_and_a_closed_stdout_do_not_end_a_run() {
    let h = Harness::new();
    let n = h.spawn(&["run", "db", "my-vm:5432"], &[("STUB_CLOSE_STDOUT", "1")]);
    h.wait_for_stub();
    // The stub has written its bad byte and closed stdout by now: a pump that
    // ended the run on either would have taken gcloud down with it
    assert!(
        h.wait_exit(n, Duration::from_millis(1500)).is_none(),
        "the run ended when gcloud's output did: {}",
        h.stdout(n)
    );
    assert!(
        h.stdout(n).contains('\u{fffd}'),
        "the bad byte is printed lossily: {:?}",
        h.stdout(n)
    );
    h.terminate(n);
    let status = h.wait_exit(n, BOUND).expect("the run ignored SIGTERM");
    assert_eq!(status.code(), Some(0), "a requested stop exits 0");
    h.assert_nothing_left();
}

#[test]
fn a_gcloud_that_exits_at_once_makes_the_run_fail_with_its_code_and_tear_down() {
    let h = Harness::new();
    let n = h.spawn(
        &["run", "db", "my-vm:5432"],
        &[("STUB_EXIT_AFTER", "0"), ("STUB_EXIT_CODE", "7")],
    );
    let status = h.wait_exit(n, BOUND).expect("the run never ended");
    assert_eq!(status.code(), Some(7), "stdout: {}", h.stdout(n));
    assert!(
        h.stdout(n).contains("gcloud exited with"),
        "stdout: {}",
        h.stdout(n)
    );
    h.assert_nothing_left();
}

#[test]
fn a_gcloud_that_exits_1_makes_the_run_exit_1() {
    let h = Harness::new();
    let n = h.spawn(&["run", "db", "my-vm:5432"], &[("STUB_EXIT_AFTER", "0")]);
    let status = h.wait_exit(n, BOUND).expect("the run never ended");
    assert_eq!(status.code(), Some(1), "stdout: {}", h.stdout(n));
    h.assert_nothing_left();
}

#[test]
fn reset_stops_a_live_run_before_removing_the_config() {
    let h = Harness::new();
    let n = h.spawn(&["run", "db", "my-vm:5432"], &[]);
    assert!(h.wait_for_records(1), "the run never published a record");
    assert!(
        poll(BOUND, || !h.pids().is_empty()),
        "the stub never started"
    );
    let (code, err) = h.run_bounded(&["reset", "--yes"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        h.wait_exit(n, BOUND).is_some(),
        "the run outlived the reset"
    );
    assert!(!h.root().join("config").exists(), "the config survived");
    for (gcloud, child) in h.pids() {
        assert!(poll(BOUND, || !alive(gcloud)), "gcloud {gcloud} survived");
        assert!(
            poll(BOUND, || !alive(child)),
            "gcloud's child {child} survived"
        );
    }
}

// The hint after a command starts `update --check` detached when the cache is
// missing, and reset has just deleted the cache with everything else. That
// child writes the cache a moment after reset exited, so the directory comes
// back. The window is short, so the test watches for a while: a child that
// never started cannot be told from one that is late except by waiting.
#[test]
fn reset_leaves_no_config_directory_behind_even_a_moment_later() {
    let h = Harness::new();
    let (code, err) = h.run_bounded(&["reset", "--yes"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        !poll(Duration::from_secs(3), || h.root().join("config").exists()),
        "something recreated the config after reset removed it"
    );
}

#[test]
fn reset_removes_nothing_when_the_runs_cannot_be_listed() {
    let h = Harness::new();
    // A file where the runs directory belongs: it can be neither created nor read
    fs::write(h.root().join("config/run"), "").unwrap();
    let (code, err) = h.run_bounded(&["reset", "--yes"]);
    assert_ne!(code, Some(0), "{err}");
    assert!(err.contains("Could not list the running tunnels"), "{err}");
    assert!(err.contains("config/run"), "{err}");
    assert!(
        h.root().join("config/config.toml").exists(),
        "the config was removed"
    );
}
