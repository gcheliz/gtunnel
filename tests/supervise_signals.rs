// `__supervise` driven through the real binary. What these prove is what the
// service manager sees: a stop is answered at once wherever the supervisor
// is, and a gcloud that dies leaves nothing holding the port.
//
// Every wait is a bounded poll on the condition; a sleep never stands in for
// one, so a slow machine costs time and not a flake.

use std::fs;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 sends nothing; the pid was read from a child we made.
    unsafe { libc::kill(pid, 0) == 0 }
}

struct Supervisor {
    child: Child,
    _dir: tempfile::TempDir,
    dir: std::path::PathBuf,
}

impl Supervisor {
    // The draw is pinned to 0, so the first start and every backoff are
    // instant, unless the test is about the wait itself
    fn start(script: &str, extra: &[&str]) -> Self {
        Self::start_pinned(script, extra, "0")
    }

    fn start_pinned(script: &str, extra: &[&str], jitter: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let child = Command::new(env!("CARGO_BIN_EXE_gtunnel"))
            .arg("__supervise")
            .args(
                extra
                    .iter()
                    .map(|a| a.replace("{dir}", path.to_str().unwrap())),
            )
            .args([
                "--",
                "sh",
                "-c",
                &script.replace("{dir}", path.to_str().unwrap()),
            ])
            .env("GTUNNEL_CONFIG_DIR", path.join("config"))
            .env("GTUNNEL_NO_NOTIFY", "1")
            .env("GTUNNEL_TEST_JITTER", jitter)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn gtunnel");
        Self {
            child,
            dir: path,
            _dir: dir,
        }
    }

    fn term(&self) {
        // SAFETY: the pid is our own live child's.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) };
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_stop_during_the_restart_backoff_is_answered_at_once() {
    // exit 1 inside min_uptime spends the budget, and the first backoff is 10s:
    // pinned to the top of the draw, so it is not a wait of 0.2 s by chance
    let mut sup = Supervisor::start_pinned(
        "exit 1",
        &["--crash-marker", "{dir}/run/com.gtunnel.dev.db.crash"],
        "1",
    );
    // Past the child's exit and into the sleep
    assert!(
        poll(BOUND, || sup
            .dir
            .join("run/com.gtunnel.dev.db.restarts")
            .exists()),
        "the run was judged"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(sup.child.try_wait().unwrap().is_none(), "backing off");
    let sent = Instant::now();
    sup.term();
    assert!(
        poll(Duration::from_secs(1), || sup
            .child
            .try_wait()
            .unwrap()
            .is_some()),
        "still alive {:?} after SIGTERM",
        sent.elapsed()
    );
}

#[test]
fn a_group_member_that_outlives_gcloud_is_swept_within_the_grace() {
    let mut sup = Supervisor::start("sleep 30 >/dev/null & echo $! > {dir}/member; exit 3", &[]);
    assert!(poll(BOUND, || fs::read_to_string(sup.dir.join("member"))
        .is_ok_and(|t| t.ends_with('\n'))));
    let sleeper: i32 = fs::read_to_string(sup.dir.join("member"))
        .expect("member")
        .trim()
        .parse()
        .expect("the member's pid");
    let began = Instant::now();
    let bound = Duration::from_secs(2) + Duration::from_secs(1);
    assert!(
        poll(bound, || sup.child.try_wait().unwrap().is_some()),
        "the supervisor waited on a lingering member"
    );
    assert!(began.elapsed() <= bound);
    assert!(
        poll(Duration::from_secs(1), || !alive(sleeper)),
        "the member was left holding the group"
    );
}

#[test]
fn a_group_left_by_a_killed_supervisor_is_swept_before_the_next_spawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ready = dir.path().join("com.gtunnel.dev.db.ready");
    // A leader that exits and leaves a member in its group: what a
    // SIGKILLed supervisor leaves, whose sweep never ran
    let mut leader = {
        use std::os::unix::process::CommandExt;
        Command::new("sh")
            .args(["-c", "sleep 30 >/dev/null & echo $!"])
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .expect("leader")
    };
    let mut text = String::new();
    leader
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .expect("read");
    leader.wait().expect("reap");
    let stale: i32 = text.trim().parse().expect("pid");
    assert!(alive(stale));
    fs::write(
        dir.path().join("com.gtunnel.dev.db.pgid"),
        format!("{} 0.000000\n", leader.id()),
    )
    .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_gtunnel"))
        .arg("__supervise")
        .arg("--ready-file")
        .arg(&ready)
        .args(["--", "true"])
        .env("GTUNNEL_CONFIG_DIR", dir.path().join("config"))
        .env("GTUNNEL_NO_NOTIFY", "1")
        .env("GTUNNEL_TEST_JITTER", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run");
    // `true` quit by itself, which the supervisor reports as a failure so the
    // service manager starts it again
    assert_eq!(status.code(), Some(1));
    assert!(
        poll(Duration::from_secs(1), || !alive(stale)),
        "the stale group's member survived the next supervisor"
    );
    assert!(!dir.path().join("com.gtunnel.dev.db.pgid").exists());
}
