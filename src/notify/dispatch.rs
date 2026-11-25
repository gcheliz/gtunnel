use super::backend::Step;

/// The line a chain that delivered nothing leaves behind, naming each step
/// and why it failed.
fn failure_line(failures: &[(String, String)]) -> String {
    let each: Vec<String> = failures
        .iter()
        .map(|(step, why)| format!("{step}: {why}"))
        .collect();
    format!("notification not delivered: {}", each.join("; "))
}

/// A chain's thread, which ends with the failure line when nothing delivered.
pub(super) type Chain = std::thread::JoinHandle<Option<String>>;

/// The chain runs on a thread of its own: it is called from the TUI event
/// loop, and three notifiers each allowed their bound would otherwise freeze
/// the screen for fifteen seconds. The first to succeed ends the chain.
pub(super) fn spawn_chain(chain: Vec<Step>) -> Chain {
    std::thread::spawn(move || {
        let mut failures = Vec::new();
        for step in chain {
            match step.run() {
                Ok(()) => return None,
                Err(why) => failures.push((step.name().to_string(), why)),
            }
        }
        Some(failure_line(&failures))
    })
}

/// The chains still delivering, so the process can wait for them before it
/// exits, and the last failure among those that ended.
///
/// The watchdog is a oneshot job: its pass ends milliseconds after it
/// announces, and systemd (and launchd) kill whatever the job left running,
/// a `notify-send` already spawned included. The change is in the ledger as
/// announced by then, so a dropped chain is an alert lost for good.
pub(super) struct Pending(std::sync::Mutex<Chains>);

pub(super) struct Chains {
    pub(super) running: Vec<Chain>,
    pub(super) failure: Option<String>,
}

impl Chains {
    /// Keeps a finished chain's failure, then lets the chain go
    fn reap(&mut self, chain: Chain) {
        if let Ok(Some(line)) = chain.join() {
            self.failure = Some(line);
        }
    }
}

impl Pending {
    pub(super) const fn new() -> Self {
        Pending(std::sync::Mutex::new(Chains {
            running: Vec::new(),
            failure: None,
        }))
    }

    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, Chains> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn track(&self, chain: Chain) {
        let mut chains = self.lock();
        // The TUI runs for hours; a finished chain is not kept for its exit
        let (done, running): (Vec<_>, Vec<_>) = std::mem::take(&mut chains.running)
            .into_iter()
            .partition(|c| c.is_finished());
        for chain in done {
            chains.reap(chain);
        }
        chains.running = running;
        chains.running.push(chain);
    }

    /// Waits for every chain, all of them within `bound` together: a notifier
    /// that hangs must not hold the exit, so one still running is abandoned.
    /// Returns the last failure, once: a pass that failed to notify five times
    /// says so in one line.
    pub(super) fn flush_within(&self, bound: std::time::Duration) -> Option<String> {
        let running = std::mem::take(&mut self.lock().running);
        let deadline = std::time::Instant::now() + bound;
        let mut chains = self.lock();
        for chain in running {
            while !chain.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if chain.is_finished() {
                chains.reap(chain);
            }
        }
        chains.failure.take()
    }
}

pub(super) static PENDING: Pending = Pending::new();

/// Lets the notifications already sent finish before the process exits, and
/// says on stderr when one could not be delivered.
///
/// Called where the process ends, never from the TUI loop, which must not
/// wait on a notifier. Printed here and never from the chain's thread: by
/// now the TUI has given the terminal back, and the watchdog's stderr is its
/// log.
#[allow(clippy::print_stderr)]
pub fn flush() {
    if let Some(line) = PENDING.flush_within(crate::proc::NOTIFY_BOUND) {
        eprintln!("{line}");
    }
}
