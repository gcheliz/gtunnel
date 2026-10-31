// The one harness every binary-level test builds on. Each file of tests/ is its
// own crate, so a file takes it with `mod common;` and uses the part it needs;
// the allow below is for the parts it does not.
//
// A Harness is a throwaway root holding a `bin/` with stub programs first on
// PATH, a `config/` that GTUNNEL_CONFIG_DIR points at, and a fresh update-check
// cache so the post-command hint never reaches for the network. The binary runs
// with HOME inside that root, so nothing it writes lands in the real config.
#![allow(dead_code)]

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use wiremock::MockServer;

/// A gcloud that is logged in and otherwise does nothing.
pub const LOGGED_IN_STUB: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token")
    echo stub-token
    exit 0
    ;;
esac
exit 0
"#;

pub const CONFIG: &str = r#"selected_account = "demo"

[[accounts]]
name = "demo"
gcloud_configuration = "work"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"
"#;

pub const TUNNELS: &str = r#"[[tunnels]]
name = "db"
account_name = "demo"
auto_start = false

[tunnels.spec]
mode = "access"
remote_port = 5432
local_port = 15432

[tunnels.spec.target]
kind = "instance"
instance = "postgres-1"
zone = "europe-west1-b"
"#;

pub const BOUND: Duration = Duration::from_secs(15);

/// Calls `done` every 50 ms until it holds or `bound` passes. A sleep never
/// stands in for a condition: a slow machine then costs time, not a flake.
pub fn poll(bound: Duration, mut done: impl FnMut() -> bool) -> bool {
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

/// Whether `pid` names a live process. Signal 0 sends nothing, so a pid that was
/// read from a stub's own file or from a child the test made is only looked at.
pub fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; every caller passes a pid it owns.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A process that outlives the test body and is not the test process, for a
/// run record that must read as live. The caller kills and waits for it.
pub fn sleeper() -> Child {
    Command::new("/bin/sleep").arg("30").spawn().expect("sleep")
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs()
}

/// A run record as `gtunnel run` writes it, under `<root>/config/run`. The pid
/// decides whether the run counts as live, so the caller picks one that is or
/// is not; with no start token, liveness is the pid alone, as for a record
/// written before the token existed. `started_at` is the caller's because a
/// run whose TTL has passed is one `list` stops, and a fixture that means
/// "live" must not be expired.
pub fn write_run_record(
    root: &Path,
    account: &str,
    name: &str,
    pid: u32,
    started_at: u64,
    ttl_secs: Option<u64>,
    spec_json: &str,
) {
    let dir = root.join("config/run");
    fs::create_dir_all(&dir).unwrap();
    let ttl = ttl_secs
        .map(|t| format!(r#","ttl_secs":{t}"#))
        .unwrap_or_default();
    fs::write(
        dir.join(format!("{account}-{name}.json")),
        format!(
            r#"{{"name":"{name}","account_name":"{account}","pid":{pid},"started_at":{started_at},"spec":{spec_json}{ttl}}}"#
        ),
    )
    .unwrap();
}

pub struct Harness {
    pub dir: TempDir,
    envs: Vec<(String, String)>,
    // The default PATH is the stubs and the system directories only, so a
    // test never finds a developer's own gcloud; a few tests need the real one
    inherit_path: bool,
    children: RefCell<Vec<Child>>,
}

impl Harness {
    /// A logged-in-or-not `gcloud`, the `demo` account and its one saved tunnel.
    pub fn with_stub(stub: &str) -> Self {
        Self::with_stubs(&[("gcloud", stub)])
    }

    /// Each stub is an executable of that name in the first directory on PATH,
    /// so it stands in for the real program in the run.
    pub fn with_stubs(stubs: &[(&str, &str)]) -> Self {
        Self::build(stubs, CONFIG, Some(TUNNELS))
    }

    /// Everything the shorter constructors fill in, spelled out. `tunnels` of
    /// `None` leaves no tunnels.toml, as a first run would find it.
    pub fn build(stubs: &[(&str, &str)], config: &str, tunnels: Option<&str>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for sub in ["bin", "config", "stub"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        for (name, body) in stubs {
            let path = root.join("bin").join(name);
            fs::write(&path, body).unwrap();
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        fs::write(root.join("config/config.toml"), config).unwrap();
        if let Some(tunnels) = tunnels {
            fs::write(root.join("config/tunnels.toml"), tunnels).unwrap();
        }
        fs::write(
            root.join("config/update-check.json"),
            r#"{"latest_version":"0.0.1","checked_at":99999999999}"#,
        )
        .unwrap();
        Self {
            dir,
            envs: Vec::new(),
            inherit_path: false,
            children: RefCell::new(Vec::new()),
        }
    }

    /// An environment variable for every run of this harness.
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.envs.push((key.to_string(), value.to_string()));
        self
    }

    /// Keep the caller's PATH after the stubs instead of the system directories.
    pub fn with_inherited_path(mut self) -> Self {
        self.inherit_path = true;
        self
    }

    /// Point every GCP API of the binary at a wiremock server. Only a debug
    /// build honours it, which is the build `cargo test` makes.
    pub fn with_gcp(self, server: &MockServer) -> Self {
        self.with_env("GTUNNEL_TEST_GCP_ENDPOINT", &server.uri())
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn config_path(&self, file: &str) -> std::path::PathBuf {
        self.root().join("config").join(file)
    }

    pub fn command(&self, args: &[&str], envs: &[(&str, &str)]) -> Command {
        let system = if self.inherit_path {
            std::env::var("PATH").unwrap_or_default()
        } else {
            "/usr/bin:/bin:/usr/sbin:/sbin".to_string()
        };
        let path = format!("{}:{system}", self.root().join("bin").display());
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gtunnel"));
        cmd.args(args)
            .env("PATH", path)
            .env("HOME", self.root())
            .env("GTUNNEL_CONFIG_DIR", self.root().join("config"))
            .env("GTUNNEL_NO_NOTIFY", "1")
            .env("STUB_DIR", self.root().join("stub"))
            .stdin(Stdio::null());
        for (k, v) in self.envs.iter() {
            cmd.env(k, v);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd
    }

    /// Run to completion and collect the output. A run that never ends hangs
    /// the test, so a command that stays belongs to `spawn`.
    pub fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    pub fn run_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        self.command(args, envs).output().expect("run gtunnel")
    }

    /// Start a run that stays; its output goes to `out-<n>` and `err-<n>` so a
    /// full pipe can never stall it. Returns its index. `Drop` ends whatever is
    /// left, so a failed assertion leaks no process.
    pub fn spawn(&self, args: &[&str], envs: &[(&str, &str)]) -> usize {
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

    /// The index the next spawned or adopted child will have, for a test that
    /// names its own output files before it starts one.
    pub fn next_index(&self) -> usize {
        self.children.borrow().len()
    }

    /// Hand a child the test started itself to the harness, so `Drop` ends it.
    pub fn adopt(&self, child: Child) -> usize {
        let n = self.next_index();
        self.children.borrow_mut().push(child);
        n
    }

    pub fn stdout(&self, n: usize) -> String {
        fs::read_to_string(self.root().join(format!("out-{n}"))).unwrap_or_default()
    }

    pub fn stderr(&self, n: usize) -> String {
        fs::read_to_string(self.root().join(format!("err-{n}"))).unwrap_or_default()
    }

    pub fn pid(&self, n: usize) -> i32 {
        self.children.borrow()[n].id() as i32
    }

    pub fn wait_exit(&self, n: usize, bound: Duration) -> Option<ExitStatus> {
        let mut status = None;
        poll(bound, || {
            status = self.children.borrow_mut()[n].try_wait().unwrap();
            status.is_some()
        });
        status
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
        // The stub's own processes, should a test have failed halfway; a stub
        // that records `<pid> <child pid>` lines in stub/pids is covered
        let pids = fs::read_to_string(self.dir.path().join("stub/pids")).unwrap_or_default();
        for line in pids.lines() {
            let Some((a, b)) = line.split_once(' ') else {
                continue;
            };
            let (Ok(a), Ok(b)) = (a.parse::<i32>(), b.parse::<i32>()) else {
                continue;
            };
            // SAFETY: SIGKILL to pids this test's stub recorded.
            unsafe {
                libc::kill(a, libc::SIGKILL);
                libc::kill(b, libc::SIGKILL);
            }
        }
    }
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
