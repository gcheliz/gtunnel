// `gtunnel run` driven through the real binary with a stub `gcloud` that, like
// the real one, is a launcher: it starts a child of its own and waits for it.
// What these prove is what a person would see with `ps` after the run ends:
// neither the stub nor its child is left, however the run was stopped.
//
// Every wait is a bounded poll on the condition; a sleep never stands in for
// one, so a slow machine costs time and not a flake.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

struct Harness {
    dir: TempDir,
    children: RefCell<Vec<Child>>,
}

// The stub's child is written next to its own pid so a test can look for both.
// STUB_STUBBORN gives the child a SIGTERM handler that ignores the signal, as
// an ssh holding a forward may; STUB_EXIT_AFTER makes the stub itself exit
// first and leave that child behind.
const STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token") echo stub-token; exit 0 ;;
esac
[ "$1" = compute ] || exit 0
if [ -n "$STUB_STUBBORN" ]; then (trap '' TERM; exec sleep 300) & else sleep 300 & fi
echo "$$ $!" >> "$STUB_DIR/pids"
echo "Listening on port [15000]."
if [ -n "$STUB_EXIT_AFTER" ]; then sleep "$STUB_EXIT_AFTER"; exit 1; fi
wait
"#;

const CONFIG: &str = r#"selected_account = "dev"

[[accounts]]
name = "dev"
gcloud_configuration = "work"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"
"#;

const BOUND: Duration = Duration::from_secs(15);

fn poll(bound: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; the pid was read from the stub's own
    // file and is positive.
    unsafe { libc::kill(pid, 0) == 0 }
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for sub in ["bin", "stub", "config"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        let gcloud = root.join("bin/gcloud");
        fs::write(&gcloud, STUB).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&gcloud, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(root.join("config/config.toml"), CONFIG).unwrap();
        // A fresh cache keeps the post-command update hint from spawning a
        // network check out of the test
        fs::write(
            root.join("config/update-check.json"),
            r#"{"latest_version":"0.0.1","checked_at":99999999999}"#,
        )
        .unwrap();
        Self {
            dir,
            children: RefCell::new(Vec::new()),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn command(&self, args: &[&str], envs: &[(&str, &str)]) -> Command {
        let path = format!(
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            self.root().join("bin").display()
        );
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gtunnel"));
        cmd.args(args)
            .env("PATH", path)
            .env("HOME", self.root())
            .env("GTUNNEL_CONFIG_DIR", self.root().join("config"))
            .env("GTUNNEL_NO_NOTIFY", "1")
            .env("STUB_DIR", self.root().join("stub"))
            .stdin(Stdio::null());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd
    }

    /// Run to completion, bounded; for the runs that are refused and so end at
    /// once. A run that wrongly starts is left to `Drop` and reads as no exit
    /// code, so the test fails instead of hanging.
    fn run(&self, args: &[&str]) -> (Option<i32>, String) {
        let n = self.spawn(args, &[]);
        let code = self.wait_exit(n, BOUND).and_then(|s| s.code());
        let err = fs::read_to_string(self.root().join(format!("err-{n}"))).unwrap_or_default();
        (code, err)
    }

    /// Start a run that stays; its output goes to `out-<n>` and `err-<n>` so a
    /// full pipe can never stall it. Returns its index.
    fn spawn(&self, args: &[&str], envs: &[(&str, &str)]) -> usize {
        let n = self.children.borrow().len();
        let out = fs::File::create(self.root().join(format!("out-{n}"))).unwrap();
        let err = fs::File::create(self.root().join(format!("err-{n}"))).unwrap();
        let child = self
            .command(args, envs)
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("spawn gtunnel");
        self.children.borrow_mut().push(child);
        n
    }

    fn stdout(&self, n: usize) -> String {
        fs::read_to_string(self.root().join(format!("out-{n}"))).unwrap_or_default()
    }

    fn pid(&self, n: usize) -> i32 {
        self.children.borrow()[n].id() as i32
    }

    fn wait_exit(&self, n: usize, bound: Duration) -> Option<ExitStatus> {
        let mut status = None;
        poll(bound, || {
            status = self.children.borrow_mut()[n].try_wait().unwrap();
            status.is_some()
        });
        status
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

impl Drop for Harness {
    fn drop(&mut self) {
        for child in self.children.borrow_mut().iter_mut() {
            // SAFETY: SIGTERM to a gtunnel this harness spawned and still holds.
            unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
        }
        for child in self.children.borrow_mut().iter_mut() {
            poll(Duration::from_secs(10), || {
                child.try_wait().ok().flatten().is_some()
            });
            let _ = child.kill();
            let _ = child.wait();
        }
        // The stub's own processes, should a test have failed halfway
        for (gcloud, child) in self.pids() {
            // SAFETY: SIGKILL to pids this test's stub recorded.
            unsafe {
                libc::kill(gcloud, libc::SIGKILL);
                libc::kill(child, libc::SIGKILL);
            }
        }
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

    let (code, err) = h.run(&["run", "db", "my-vm:5432"]);
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
    let (code, err) = h.run(&["run", "--local-port", "0", "db", "my-vm:5432"]);
    assert_eq!(code, Some(2), "stderr: {err}");
    assert!(
        err.contains("Local port must be between 1 and 65535"),
        "stderr: {err}"
    );
    assert!(h.pids().is_empty(), "no gcloud for a refused port");
}
