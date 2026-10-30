//! The one place a setting's precedence is applied: flag, then environment,
//! then `config.toml`, then the default.
//!
//! Before this each knob read its own variable where it was used, with its own
//! idea of what a bad value is, and none of them could be set in the file. A
//! variable always wins over the same key in the file, with no key that opts
//! out, so a value edited in `config.toml` that has no effect has exactly one
//! cause, and `doctor` names it (`Resolved::overrides`).
//!
//! No setting here has a flag yet; the precedence a flag would take sits above
//! the variable and is the caller's to apply, as `--log-level` does, because a
//! flag is parsed before this module is reachable.

use std::time::Duration;

use anyhow::Result;

use crate::config::Config;
use crate::error::GtunnelError;

/// How long a start waits for an Access tunnel to report it is listening.
pub const START_DEADLINE: Duration = Duration::from_secs(20);

pub fn default_start_deadline() -> String {
    humantime::format_duration(START_DEADLINE).to_string()
}

pub fn default_service_call_timeout() -> String {
    humantime::format_duration(crate::proc::SERVICE_BOUND).to_string()
}

/// The wait for one `list` or TUI probe to connect and read an answer. Short
/// because `list` probes every running tunnel at once and so waits one
/// probe's worth however many there are; a silent close adds the probe's
/// settle time to read the log. Not `[watch] probe_timeout`, which is the
/// watchdog's wait and is generous because a false trip costs a restart.
pub const LIST_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

pub fn default_list_probe_timeout() -> String {
    humantime::format_duration(LIST_PROBE_TIMEOUT).to_string()
}

/// How old the cached release answer may get before the next TUI or CLI start
/// asks GitHub again. Six hours, not a day: a release is news the same
/// afternoon, and the check is one small request made detached.
pub const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub fn default_update_check_interval() -> String {
    humantime::format_duration(UPDATE_CHECK_INTERVAL).to_string()
}

pub fn default_discovery() -> bool {
    true
}

/// How long a healthy tunnel has to answer its own local port. Generous: a
/// slow answer is a working tunnel, and a false trip costs a restart.
pub const WATCH_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Consecutive failed probes before a tunnel is called degraded: the
/// breaker's own run length, so the watchdog and the breaker agree on what
/// "failing" means and one blip is neither.
pub const WATCH_DEGRADED_AFTER: i64 = 2;

/// How often the watchdog wakes to look at every tunnel. Short enough that a
/// wedged tunnel is noticed before a person reaches for the terminal, long
/// enough that the probe it runs costs nothing worth measuring.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(30);

pub fn default_watch_probe_timeout() -> String {
    humantime::format_duration(WATCH_PROBE_TIMEOUT).to_string()
}

pub fn default_watch_degraded_after() -> i64 {
    WATCH_DEGRADED_AFTER
}

pub fn default_watch_interval() -> String {
    humantime::format_duration(WATCH_INTERVAL).to_string()
}

/// What a tunnel's log may reach before it is trimmed. Eight MiB is days of a
/// chatty tunnel and still opens instantly in a pager.
pub const LOGS_MAX_SIZE: u64 = 8 * 1024 * 1024;

/// What survives a trim: a quarter of the cap, so a trim buys a long quiet
/// stretch and is not repeated on the next line.
pub const LOGS_KEEP_SIZE: u64 = 2 * 1024 * 1024;

/// Connection lines let through per minute. A port scan or a client that
/// reconnects in a loop must not bury the lines that explain a failure.
pub const LOGS_LINES_PER_MINUTE: i64 = 60;

/// How long before gcloud's exit a credential line still explains it.
pub const LOGS_AUTH_EXIT_WINDOW: Duration = Duration::from_secs(30);

/// Google issues access tokens for an hour; refreshing at 45 minutes keeps a
/// long operation from straddling the expiry.
pub const GCP_TOKEN_TTL: Duration = Duration::from_secs(45 * 60);

/// How long a compute or DNS operation may run before gtunnel stops waiting.
/// Ten minutes covers the slowest thing it creates, a managed certificate.
pub const GCP_OPERATION_TIMEOUT: Duration = Duration::from_secs(600);

/// One API request, connect to last byte.
pub const GCP_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Between looks at a running operation: often enough that a quick one is not
/// padded by a long sleep, rare enough not to spend the API quota.
pub const GCP_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Long enough for another gtunnel to finish a save, short enough that a
/// wedged holder is reported rather than waited on.
pub const GCP_LOCK_DEADLINE: Duration = Duration::from_secs(5);

/// Reads of a fingerprinted resource redone after a concurrent change (412).
pub const GCP_FINGERPRINT_ATTEMPTS: i64 = 5;

pub fn default_gcp_token_ttl() -> String {
    shown(GCP_TOKEN_TTL)
}

pub fn default_gcp_operation_timeout() -> String {
    shown(GCP_OPERATION_TIMEOUT)
}

pub fn default_gcp_http_timeout() -> String {
    shown(GCP_HTTP_TIMEOUT)
}

pub fn default_gcp_poll_interval() -> String {
    shown(GCP_POLL_INTERVAL)
}

pub fn default_gcp_lock_deadline() -> String {
    shown(GCP_LOCK_DEADLINE)
}

pub fn default_gcp_fingerprint_attempts() -> i64 {
    GCP_FINGERPRINT_ATTEMPTS
}

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;

pub fn default_logs_max_size() -> String {
    show_size(LOGS_MAX_SIZE)
}

pub fn default_logs_keep_size() -> String {
    show_size(LOGS_KEEP_SIZE)
}

pub fn default_logs_lines_per_minute() -> i64 {
    LOGS_LINES_PER_MINUTE
}

pub fn default_logs_auth_exit_window() -> String {
    humantime::format_duration(LOGS_AUTH_EXIT_WINDOW).to_string()
}

/// How long the TUI trusts the watchdog's last heartbeat: three passes, never
/// under the 90 s the default interval has always implied.
///
/// Derived from the interval and not a second key, because the two cannot be
/// set apart: a freshness window shorter than one pass reads a healthy
/// watchdog as dead, and one key cannot be set wrong against itself.
pub fn heartbeat_fresh_secs(interval: Duration) -> u64 {
    crate::lifecycle::HEARTBEAT_FRESH_SECS.max(interval.as_secs().saturating_mul(3))
}

/// One key's variables. The plain spelling is the one the docs name; the
/// legacy one keeps a script that already exports it working, and loses to the
/// plain one when both are set.
struct Key {
    name: &'static str,
    var: &'static str,
    legacy: Option<&'static str>,
}

const START_DEADLINE_KEY: Key = Key {
    name: "start_deadline",
    var: "GTUNNEL_START_DEADLINE",
    legacy: Some("GTUNNEL_START_DEADLINE_MS"),
};
const SERVICE_CALL_TIMEOUT_KEY: Key = Key {
    name: "service_call_timeout",
    var: "GTUNNEL_SERVICE_CALL_TIMEOUT",
    legacy: Some("GTUNNEL_SERVICE_BOUND_MS"),
};
const LIST_PROBE_TIMEOUT_KEY: Key = Key {
    name: "list_probe_timeout",
    var: "GTUNNEL_LIST_PROBE_TIMEOUT",
    legacy: None,
};
const UPDATE_CHECK_INTERVAL_KEY: Key = Key {
    name: "update_check_interval",
    var: "GTUNNEL_UPDATE_CHECK_INTERVAL",
    legacy: None,
};
const DISCOVERY_KEY: Key = Key {
    name: "discovery",
    var: "GTUNNEL_DISCOVERY",
    legacy: Some("GTUNNEL_NO_DISCOVERY"),
};
const NOTIFICATIONS_KEY: Key = Key {
    name: "notifications.enabled",
    var: "GTUNNEL_NOTIFICATIONS",
    legacy: Some("GTUNNEL_NO_NOTIFY"),
};
const WATCH_PROBE_TIMEOUT_KEY: Key = Key {
    name: "watch.probe_timeout",
    var: "GTUNNEL_WATCH_PROBE_TIMEOUT",
    legacy: None,
};
const WATCH_DEGRADED_AFTER_KEY: Key = Key {
    name: "watch.degraded_after",
    var: "GTUNNEL_WATCH_DEGRADED_AFTER",
    legacy: None,
};
const WATCH_INTERVAL_KEY: Key = Key {
    name: "watch.interval",
    var: "GTUNNEL_WATCH_INTERVAL",
    legacy: None,
};
const LOGS_MAX_SIZE_KEY: Key = Key {
    name: "logs.max_size",
    var: "GTUNNEL_LOGS_MAX_SIZE",
    legacy: None,
};
const LOGS_KEEP_SIZE_KEY: Key = Key {
    name: "logs.keep_size",
    var: "GTUNNEL_LOGS_KEEP_SIZE",
    legacy: None,
};
const LOGS_LINES_PER_MINUTE_KEY: Key = Key {
    name: "logs.lines_per_minute",
    var: "GTUNNEL_LOGS_LINES_PER_MINUTE",
    legacy: None,
};
const LOGS_AUTH_EXIT_WINDOW_KEY: Key = Key {
    name: "logs.auth_exit_window",
    var: "GTUNNEL_LOGS_AUTH_EXIT_WINDOW",
    legacy: None,
};
const GCP_TOKEN_TTL_KEY: Key = Key {
    name: "gcp.token_ttl",
    var: "GTUNNEL_GCP_TOKEN_TTL",
    legacy: None,
};
const GCP_OPERATION_TIMEOUT_KEY: Key = Key {
    name: "gcp.operation_timeout",
    var: "GTUNNEL_GCP_OPERATION_TIMEOUT",
    legacy: None,
};
const GCP_HTTP_TIMEOUT_KEY: Key = Key {
    name: "gcp.http_timeout",
    var: "GTUNNEL_GCP_HTTP_TIMEOUT",
    legacy: None,
};
const GCP_POLL_INTERVAL_KEY: Key = Key {
    name: "gcp.poll_interval",
    var: "GTUNNEL_GCP_POLL_INTERVAL",
    legacy: None,
};
const GCP_LOCK_DEADLINE_KEY: Key = Key {
    name: "gcp.lock_deadline",
    var: "GTUNNEL_GCP_LOCK_DEADLINE",
    legacy: None,
};
const GCP_FINGERPRINT_ATTEMPTS_KEY: Key = Key {
    name: "gcp.fingerprint_attempts",
    var: "GTUNNEL_GCP_FINGERPRINT_ATTEMPTS",
    legacy: None,
};
const EDITOR_KEY: Key = Key {
    name: "editor",
    var: "GTUNNEL_EDITOR",
    legacy: None,
};

/// What the process runs on after the precedence is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub start_deadline: Duration,
    pub service_call_timeout: Duration,
    pub list_probe_timeout: Duration,
    pub update_check_interval: Duration,
    pub discovery: bool,
    pub notifications: bool,
    pub watch_probe_timeout: Duration,
    pub watch_degraded_after: u32,
    /// Whole seconds: a launchd `StartInterval` and a systemd timer have no
    /// finer unit, so a finer value would be one the unit cannot honour.
    pub watch_interval: Duration,
    /// Bytes. `logs_keep_size` is always below `logs_max_size`.
    pub logs_max_size: u64,
    pub logs_keep_size: u64,
    pub logs_lines_per_minute: u32,
    pub logs_auth_exit_window: Duration,
    pub gcp_token_ttl: Duration,
    pub gcp_operation_timeout: Duration,
    pub gcp_http_timeout: Duration,
    pub gcp_poll_interval: Duration,
    pub gcp_lock_deadline: Duration,
    pub gcp_fingerprint_attempts: u32,
    /// `GTUNNEL_EDITOR`, which outranks `VISUAL` and `EDITOR` like any
    /// variable outranks the file.
    pub editor_env: Option<String>,
    /// The file's `editor`, below `VISUAL` and `EDITOR`: the one key whose
    /// file value is a fallback for other variables and not only a default.
    pub editor_file: Option<String>,
}

/// A key whose value the environment set to something other than the file's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    pub key: &'static str,
    pub variable: &'static str,
    pub file: String,
    pub effective: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub settings: Settings,
    pub overrides: Vec<Override>,
}

pub type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

/// An exported-but-empty variable is how a shell spells unset.
fn var(env: EnvFn, name: &str) -> Option<String> {
    env(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// The source is named in every error: the value may come from a shell profile
// the reader is not looking at, and a file value from a key they forgot.
fn bad(source: &str, why: String) -> anyhow::Error {
    GtunnelError::usage(format!("{source}: {why}")).into()
}

fn parse_duration(source: &str, raw: &str) -> Result<Duration> {
    let d = humantime::parse_duration(raw).map_err(|e| {
        bad(
            source,
            format!("'{raw}' is not a duration like 30s or 1m ({e})"),
        )
    })?;
    if d.is_zero() {
        return Err(bad(source, format!("'{raw}' must be longer than zero")));
    }
    Ok(d)
}

fn parse_ms(source: &str, raw: &str) -> Result<Duration> {
    match raw.parse::<u64>() {
        Ok(ms) if ms > 0 => Ok(Duration::from_millis(ms)),
        _ => Err(bad(
            source,
            format!("'{raw}' is not a whole number of milliseconds above zero"),
        )),
    }
}

fn parse_bool(source: &str, raw: &str) -> Result<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        _ => Err(bad(source, format!("'{raw}' is not true or false"))),
    }
}

fn shown(d: Duration) -> String {
    humantime::format_duration(d).to_string()
}

/// A duration key: the file's value is always checked, so a bad one is refused
/// whatever the environment says; then a variable replaces it.
fn duration_key(
    env: EnvFn,
    key: &Key,
    file_raw: &str,
    overrides: &mut Vec<Override>,
) -> Result<Duration> {
    let file = parse_duration(key.name, file_raw)?;
    let used = if let Some(raw) = var(env, key.var) {
        Some((key.var, parse_duration(key.var, &raw)?))
    } else if let Some(legacy) = key.legacy
        && let Some(raw) = var(env, legacy)
    {
        Some((legacy, parse_ms(legacy, &raw)?))
    } else {
        None
    };
    let Some((variable, effective)) = used else {
        return Ok(file);
    };
    if effective != file {
        overrides.push(Override {
            key: key.name,
            variable,
            file: shown(file),
            effective: shown(effective),
        });
    }
    Ok(effective)
}

/// A whole-second duration, for the one value a service manager reads.
fn parse_whole_seconds(source: &str, raw: &str) -> Result<Duration> {
    let d = parse_duration(source, raw)?;
    if d.subsec_nanos() != 0 {
        return Err(bad(
            source,
            format!(
                "'{raw}' must be a whole number of seconds, the service manager has no finer unit"
            ),
        ));
    }
    Ok(d)
}

/// A byte count: bare bytes, or a number with `B`, `KiB`, `MiB` or `GiB`.
/// Binary units only, because a log cap of "8MB" that means 8,000,000 to one
/// reader and 8,388,608 to another is the ambiguity a setting must not have.
fn parse_size(source: &str, raw: &str) -> Result<u64> {
    let lower = raw.trim().to_ascii_lowercase();
    let split = lower
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(lower.len());
    let (digits, unit) = lower.split_at(split);
    let factor = match unit.trim() {
        "" | "b" => Some(1),
        "kib" => Some(KIB),
        "mib" => Some(MIB),
        "gib" => Some(GIB),
        _ => None,
    };
    match (digits.parse::<u64>().ok().filter(|n| *n > 0), factor) {
        (Some(n), Some(f)) => n
            .checked_mul(f)
            .ok_or_else(|| bad(source, format!("'{raw}' is too large"))),
        _ => Err(bad(
            source,
            format!("'{raw}' is not a size above zero like 8MiB, 512KiB or 1048576 bytes"),
        )),
    }
}

/// The largest binary unit that divides the count exactly, so a value read
/// back from the file prints as it was written.
fn show_size(bytes: u64) -> String {
    match bytes {
        n if n % GIB == 0 => format!("{}GiB", n / GIB),
        n if n % MIB == 0 => format!("{}MiB", n / MIB),
        n if n % KIB == 0 => format!("{}KiB", n / KIB),
        n => format!("{n}B"),
    }
}

fn parse_count(source: &str, raw: &str) -> Result<u32> {
    match raw.parse::<i64>() {
        Ok(n) if (1..=i64::from(u32::MAX)).contains(&n) => Ok(n as u32),
        _ => Err(bad(
            source,
            format!("'{raw}' is not a whole number from 1 to {}", u32::MAX),
        )),
    }
}

/// A key with one variable and no older spelling: the file's value is always
/// checked, then a variable replaces it. `duration_key` is this for durations
/// that also read a millisecond variable.
fn plain_key<T: PartialEq + Copy>(
    env: EnvFn,
    key: &Key,
    file_raw: &str,
    parse: fn(&str, &str) -> Result<T>,
    show: fn(T) -> String,
    overrides: &mut Vec<Override>,
) -> Result<T> {
    let file = parse(key.name, file_raw)?;
    let Some(raw) = var(env, key.var) else {
        return Ok(file);
    };
    let effective = parse(key.var, &raw)?;
    if effective != file {
        overrides.push(Override {
            key: key.name,
            variable: key.var,
            file: show(file),
            effective: show(effective),
        });
    }
    Ok(effective)
}

/// A switch whose legacy variable is an off switch (`GTUNNEL_NO_*`): any
/// value but empty or `0` turns the feature off, and `0` is not an opinion.
fn switch_key(env: EnvFn, key: &Key, file: bool, overrides: &mut Vec<Override>) -> Result<bool> {
    let used = if let Some(raw) = var(env, key.var) {
        Some((key.var, parse_bool(key.var, &raw)?))
    } else if let Some(legacy) = key.legacy
        && let Some(raw) = var(env, legacy)
        && raw != "0"
    {
        Some((legacy, false))
    } else {
        None
    };
    let Some((variable, effective)) = used else {
        return Ok(file);
    };
    if effective != file {
        overrides.push(Override {
            key: key.name,
            variable,
            file: file.to_string(),
            effective: effective.to_string(),
        });
    }
    Ok(effective)
}

/// Every key resolved to one value, or the first bad source. `env` is a
/// parameter so the precedence is tested without touching the process
/// environment, which `cargo test` shares between threads.
pub fn resolve(config: &Config, env: EnvFn) -> Result<Resolved> {
    let mut overrides = Vec::new();
    let start_deadline = duration_key(
        env,
        &START_DEADLINE_KEY,
        &config.start_deadline,
        &mut overrides,
    )?;
    let service_call_timeout = duration_key(
        env,
        &SERVICE_CALL_TIMEOUT_KEY,
        &config.service_call_timeout,
        &mut overrides,
    )?;
    let list_probe_timeout = plain_key(
        env,
        &LIST_PROBE_TIMEOUT_KEY,
        &config.list_probe_timeout,
        parse_duration,
        shown,
        &mut overrides,
    )?;
    let update_check_interval = plain_key(
        env,
        &UPDATE_CHECK_INTERVAL_KEY,
        &config.update_check_interval,
        parse_duration,
        shown,
        &mut overrides,
    )?;
    let discovery = switch_key(env, &DISCOVERY_KEY, config.discovery, &mut overrides)?;
    let notifications = switch_key(
        env,
        &NOTIFICATIONS_KEY,
        config.notifications.enabled,
        &mut overrides,
    )?;
    let watch = &config.watch;
    let watch_probe_timeout = plain_key(
        env,
        &WATCH_PROBE_TIMEOUT_KEY,
        &watch.probe_timeout,
        parse_duration,
        shown,
        &mut overrides,
    )?;
    let watch_degraded_after = plain_key(
        env,
        &WATCH_DEGRADED_AFTER_KEY,
        &watch.degraded_after.to_string(),
        parse_count,
        |n| n.to_string(),
        &mut overrides,
    )?;
    let watch_interval = plain_key(
        env,
        &WATCH_INTERVAL_KEY,
        &watch.interval,
        parse_whole_seconds,
        shown,
        &mut overrides,
    )?;
    let logs = &config.logs;
    let logs_max_size = plain_key(
        env,
        &LOGS_MAX_SIZE_KEY,
        &logs.max_size,
        parse_size,
        show_size,
        &mut overrides,
    )?;
    let logs_keep_size = plain_key(
        env,
        &LOGS_KEEP_SIZE_KEY,
        &logs.keep_size,
        parse_size,
        show_size,
        &mut overrides,
    )?;
    if logs_keep_size >= logs_max_size {
        // Name the variable only when it is what set the value: a variable the
        // shell never exported is not a lead
        let set = |key: &Key| {
            var(env, key.var)
                .map(|_| format!(" (set by {})", key.var))
                .unwrap_or_default()
        };
        return Err(bad(
            "logs.keep_size",
            format!(
                "{}{} must be smaller than logs.max_size {}{}, or a trim would keep the whole log",
                show_size(logs_keep_size),
                set(&LOGS_KEEP_SIZE_KEY),
                show_size(logs_max_size),
                set(&LOGS_MAX_SIZE_KEY),
            ),
        ));
    }
    let logs_lines_per_minute = plain_key(
        env,
        &LOGS_LINES_PER_MINUTE_KEY,
        &logs.lines_per_minute.to_string(),
        parse_count,
        |n| n.to_string(),
        &mut overrides,
    )?;
    let logs_auth_exit_window = plain_key(
        env,
        &LOGS_AUTH_EXIT_WINDOW_KEY,
        &logs.auth_exit_window,
        parse_duration,
        shown,
        &mut overrides,
    )?;
    let gcp = &config.gcp;
    let mut gcp_duration = |key: &Key, file_raw: &str| {
        plain_key(env, key, file_raw, parse_duration, shown, &mut overrides)
    };
    let gcp_token_ttl = gcp_duration(&GCP_TOKEN_TTL_KEY, &gcp.token_ttl)?;
    let gcp_operation_timeout = gcp_duration(&GCP_OPERATION_TIMEOUT_KEY, &gcp.operation_timeout)?;
    let gcp_http_timeout = gcp_duration(&GCP_HTTP_TIMEOUT_KEY, &gcp.http_timeout)?;
    let gcp_poll_interval = gcp_duration(&GCP_POLL_INTERVAL_KEY, &gcp.poll_interval)?;
    let gcp_lock_deadline = gcp_duration(&GCP_LOCK_DEADLINE_KEY, &gcp.lock_deadline)?;
    let gcp_fingerprint_attempts = plain_key(
        env,
        &GCP_FINGERPRINT_ATTEMPTS_KEY,
        &gcp.fingerprint_attempts.to_string(),
        parse_count,
        |n| n.to_string(),
        &mut overrides,
    )?;
    let editor_file = Some(config.editor.trim().to_string()).filter(|e| !e.is_empty());
    let editor_env = var(env, EDITOR_KEY.var);
    if let Some(chosen) = &editor_env
        && Some(chosen) != editor_file.as_ref()
    {
        overrides.push(Override {
            key: EDITOR_KEY.name,
            variable: EDITOR_KEY.var,
            file: editor_file.clone().unwrap_or_default(),
            effective: chosen.clone(),
        });
    }
    Ok(Resolved {
        settings: Settings {
            start_deadline,
            service_call_timeout,
            list_probe_timeout,
            update_check_interval,
            discovery,
            notifications,
            watch_probe_timeout,
            watch_degraded_after,
            watch_interval,
            logs_max_size,
            logs_keep_size,
            logs_lines_per_minute,
            logs_auth_exit_window,
            gcp_token_ttl,
            gcp_operation_timeout,
            gcp_http_timeout,
            gcp_poll_interval,
            gcp_lock_deadline,
            gcp_fingerprint_attempts,
            editor_env,
            editor_file,
        },
        overrides,
    })
}

/// The settings for code deep in a command that cannot return a usage error.
///
/// Reads `config.toml` on each call and not once: the TUI edits the file while
/// it runs, and a value changed there should apply on the next start, not the
/// next launch. The cost is one small read beside a `launchctl` or `ps`
/// call. Every failure degrades instead of stopping, in three steps: the file
/// with the environment, the file alone, the defaults. `dispatch` has already
/// refused a bad value for a foreground command, so reaching a fallback here
/// means the supervisor, which must not die of a typo and restart into it.
pub fn current() -> Settings {
    let config = std::fs::read_to_string(crate::config::config_path().unwrap_or_default())
        .ok()
        .and_then(|text| toml::from_str::<Config>(&text).ok())
        .unwrap_or_default();
    let real = |name: &str| std::env::var(name).ok();
    resolve(&config, &real)
        .or_else(|_| resolve(&config, &|_| None))
        .or_else(|_| resolve(&Config::default(), &|_| None))
        .map(|r| r.settings)
        .expect("the defaults are valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_with(text: &str, vars: &[(&str, &str)]) -> Result<Resolved> {
        let config: Config = toml::from_str(text).expect("config parses");
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        resolve(&config, &move |name| {
            vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        })
    }

    fn settings_of(text: &str, vars: &[(&str, &str)]) -> Settings {
        resolve_with(text, vars).expect("resolves").settings
    }

    fn exit_and_text(err: &anyhow::Error) -> (i32, String) {
        (crate::error::kind_of(err).exit_code(), format!("{err:#}"))
    }

    #[test]
    fn absent_keys_give_the_documented_defaults() {
        let s = settings_of("", &[]);
        assert_eq!(s.start_deadline, Duration::from_secs(20));
        assert_eq!(s.service_call_timeout, Duration::from_secs(10));
        assert!(s.discovery);
        assert!(!s.notifications, "notifications are off until turned on");
        assert_eq!(s.editor_env, None);
        assert_eq!(s.editor_file, None);
        assert!(resolve_with("", &[]).expect("ok").overrides.is_empty());
    }

    #[test]
    fn the_file_beats_the_default() {
        let s = settings_of(
            "start_deadline = \"45s\"\ndiscovery = false\nservice_call_timeout = \"2s\"\n\
             editor = \"nano\"\n[notifications]\nenabled = false\n",
            &[],
        );
        assert_eq!(s.start_deadline, Duration::from_secs(45));
        assert_eq!(s.service_call_timeout, Duration::from_secs(2));
        assert!(!s.discovery);
        assert!(!s.notifications);
        assert_eq!(s.editor_file.as_deref(), Some("nano"));
    }

    #[test]
    fn a_variable_beats_the_same_key_in_the_file() {
        let file = "start_deadline = \"45s\"\ndiscovery = false\nservice_call_timeout = \"2s\"\n\
                    editor = \"nano\"\n[notifications]\nenabled = false\n";
        let s = settings_of(
            file,
            &[
                ("GTUNNEL_START_DEADLINE", "90s"),
                ("GTUNNEL_SERVICE_CALL_TIMEOUT", "500ms"),
                ("GTUNNEL_DISCOVERY", "true"),
                ("GTUNNEL_NOTIFICATIONS", "yes"),
                ("GTUNNEL_EDITOR", "code -w"),
            ],
        );
        assert_eq!(s.start_deadline, Duration::from_secs(90));
        assert_eq!(s.service_call_timeout, Duration::from_millis(500));
        assert!(s.discovery);
        assert!(s.notifications);
        assert_eq!(s.editor_env.as_deref(), Some("code -w"));
        assert_eq!(s.editor_file.as_deref(), Some("nano"));
    }

    #[test]
    fn the_variables_that_already_existed_still_work_and_beat_the_file() {
        let file = "start_deadline = \"45s\"\nservice_call_timeout = \"2s\"\n\
                    [notifications]\nenabled = true\n";
        let s = settings_of(
            file,
            &[
                ("GTUNNEL_START_DEADLINE_MS", "1500"),
                ("GTUNNEL_SERVICE_BOUND_MS", "250"),
                ("GTUNNEL_NO_DISCOVERY", "1"),
                ("GTUNNEL_NO_NOTIFY", "1"),
            ],
        );
        assert_eq!(s.start_deadline, Duration::from_millis(1500));
        assert_eq!(s.service_call_timeout, Duration::from_millis(250));
        assert!(!s.discovery);
        assert!(!s.notifications);
    }

    #[test]
    fn the_plain_spelling_beats_the_legacy_one() {
        let s = settings_of(
            "",
            &[
                ("GTUNNEL_START_DEADLINE", "30s"),
                ("GTUNNEL_START_DEADLINE_MS", "1"),
                ("GTUNNEL_DISCOVERY", "true"),
                ("GTUNNEL_NO_DISCOVERY", "1"),
            ],
        );
        assert_eq!(s.start_deadline, Duration::from_secs(30));
        assert!(s.discovery);
    }

    #[test]
    fn a_legacy_off_switch_set_to_zero_or_empty_is_unset() {
        let s = settings_of(
            "discovery = false\n",
            &[("GTUNNEL_NO_DISCOVERY", "0"), ("GTUNNEL_NO_NOTIFY", " ")],
        );
        assert!(!s.discovery, "0 says nothing, so the file decides");
        assert!(!s.notifications);
    }

    #[test]
    fn a_legacy_off_switch_reads_anything_else_as_off() {
        let s = settings_of(
            "",
            &[
                ("GTUNNEL_NO_DISCOVERY", "yes"),
                ("GTUNNEL_NO_NOTIFY", "true"),
            ],
        );
        assert!(!s.discovery);
        assert!(!s.notifications);
    }

    #[test]
    fn an_empty_variable_is_how_a_shell_spells_unset() {
        let s = settings_of(
            "start_deadline = \"45s\"\n",
            &[("GTUNNEL_START_DEADLINE", ""), ("GTUNNEL_EDITOR", "  ")],
        );
        assert_eq!(s.start_deadline, Duration::from_secs(45));
        assert_eq!(s.editor_env, None);
    }

    #[test]
    fn a_bad_variable_is_a_usage_error_naming_it() {
        for (var, value) in [
            ("GTUNNEL_START_DEADLINE", "soon"),
            ("GTUNNEL_START_DEADLINE", "0s"),
            ("GTUNNEL_START_DEADLINE_MS", "soon"),
            ("GTUNNEL_START_DEADLINE_MS", "0"),
            ("GTUNNEL_SERVICE_CALL_TIMEOUT", "-3"),
            ("GTUNNEL_SERVICE_BOUND_MS", "ten"),
            ("GTUNNEL_DISCOVERY", "maybe"),
            ("GTUNNEL_NOTIFICATIONS", "2"),
        ] {
            let err = resolve_with("", &[(var, value)]).expect_err(var);
            let (code, text) = exit_and_text(&err);
            assert_eq!(code, 2, "{var}={value}: {text}");
            assert!(text.contains(var), "{var}={value}: {text}");
            assert!(text.contains(value), "{var}={value}: {text}");
        }
    }

    #[test]
    fn a_bad_variable_is_refused_even_when_the_file_has_a_good_value() {
        let err = resolve_with(
            "start_deadline = \"45s\"\n",
            &[("GTUNNEL_START_DEADLINE", "soon")],
        )
        .expect_err("the variable always wins, so it is always checked");
        assert!(format!("{err:#}").contains("GTUNNEL_START_DEADLINE"));
    }

    #[test]
    fn a_bad_file_value_is_a_usage_error_naming_the_key() {
        for (text, key) in [
            ("start_deadline = \"soon\"\n", "start_deadline"),
            ("start_deadline = \"0s\"\n", "start_deadline"),
            ("service_call_timeout = \"0s\"\n", "service_call_timeout"),
            ("service_call_timeout = \"x\"\n", "service_call_timeout"),
        ] {
            let err = resolve_with(text, &[]).expect_err(text);
            let (code, msg) = exit_and_text(&err);
            assert_eq!(code, 2, "{msg}");
            assert!(msg.contains(key), "{msg}");
        }
    }

    #[test]
    fn a_variable_that_differs_from_the_file_is_reported_as_an_override() {
        let r = resolve_with(
            "start_deadline = \"45s\"\n",
            &[("GTUNNEL_START_DEADLINE_MS", "1500")],
        )
        .expect("resolves");
        assert_eq!(
            r.overrides,
            vec![Override {
                key: "start_deadline",
                variable: "GTUNNEL_START_DEADLINE_MS",
                file: "45s".to_string(),
                effective: "1s 500ms".to_string(),
            }]
        );
    }

    #[test]
    fn a_variable_that_agrees_with_the_file_overrides_nothing() {
        let r = resolve_with(
            "discovery = false\n",
            &[
                ("GTUNNEL_NO_DISCOVERY", "1"),
                ("GTUNNEL_START_DEADLINE", "20s"),
            ],
        )
        .expect("resolves");
        assert!(r.overrides.is_empty(), "{:?}", r.overrides);
    }

    #[test]
    fn every_key_reports_its_own_variable() {
        let r = resolve_with(
            "editor = \"nano\"\n",
            &[
                ("GTUNNEL_SERVICE_CALL_TIMEOUT", "1s"),
                ("GTUNNEL_DISCOVERY", "false"),
                ("GTUNNEL_NOTIFICATIONS", "on"),
                ("GTUNNEL_EDITOR", "vim"),
            ],
        )
        .expect("resolves");
        let seen: Vec<(&str, &str)> = r.overrides.iter().map(|o| (o.key, o.variable)).collect();
        assert_eq!(
            seen,
            vec![
                ("service_call_timeout", "GTUNNEL_SERVICE_CALL_TIMEOUT"),
                ("discovery", "GTUNNEL_DISCOVERY"),
                ("notifications.enabled", "GTUNNEL_NOTIFICATIONS"),
                ("editor", "GTUNNEL_EDITOR"),
            ]
        );
    }

    #[test]
    fn the_list_probe_wait_defaults_to_what_the_constant_was() {
        let s = settings_of("", &[]);
        assert_eq!(s.list_probe_timeout, Duration::from_millis(500));
    }

    #[test]
    fn the_file_sets_the_list_probe_wait() {
        let s = settings_of("list_probe_timeout = \"1s\"\n", &[]);
        assert_eq!(s.list_probe_timeout, Duration::from_secs(1));
    }

    #[test]
    fn a_variable_beats_the_list_probe_wait_in_the_file_and_doctor_can_name_it() {
        let r = resolve_with(
            "list_probe_timeout = \"1s\"\n",
            &[("GTUNNEL_LIST_PROBE_TIMEOUT", "2s")],
        )
        .expect("resolves");
        assert_eq!(r.settings.list_probe_timeout, Duration::from_secs(2));
        assert_eq!(
            r.overrides,
            vec![Override {
                key: "list_probe_timeout",
                variable: "GTUNNEL_LIST_PROBE_TIMEOUT",
                file: "1s".to_string(),
                effective: "2s".to_string(),
            }]
        );
    }

    #[test]
    fn a_bad_list_probe_variable_is_a_usage_error_naming_it() {
        for value in ["soon", "0s", "-1", "0"] {
            let err = resolve_with("", &[("GTUNNEL_LIST_PROBE_TIMEOUT", value)]).expect_err(value);
            let (code, text) = exit_and_text(&err);
            assert_eq!(code, 2, "{value}: {text}");
            assert!(
                text.contains("GTUNNEL_LIST_PROBE_TIMEOUT"),
                "{value}: {text}"
            );
        }
    }

    #[test]
    fn a_bad_list_probe_file_value_is_a_usage_error_naming_the_key() {
        for text in [
            "list_probe_timeout = \"0s\"\n",
            "list_probe_timeout = \"x\"\n",
        ] {
            let err = resolve_with(text, &[]).expect_err(text);
            let (code, msg) = exit_and_text(&err);
            assert_eq!(code, 2, "{msg}");
            assert!(msg.contains("list_probe_timeout"), "{msg}");
        }
    }

    #[test]
    fn the_update_check_defaults_to_six_hours_and_the_file_sets_it() {
        assert_eq!(
            settings_of("", &[]).update_check_interval,
            Duration::from_secs(6 * 3600)
        );
        let s = settings_of("update_check_interval = \"12h\"\n", &[]);
        assert_eq!(s.update_check_interval, Duration::from_secs(12 * 3600));
    }

    #[test]
    fn a_variable_beats_the_update_check_interval_in_the_file_and_doctor_can_name_it() {
        let r = resolve_with(
            "update_check_interval = \"12h\"\n",
            &[("GTUNNEL_UPDATE_CHECK_INTERVAL", "1h")],
        )
        .expect("resolves");
        assert_eq!(r.settings.update_check_interval, Duration::from_secs(3600));
        assert_eq!(
            r.overrides,
            vec![Override {
                key: "update_check_interval",
                variable: "GTUNNEL_UPDATE_CHECK_INTERVAL",
                file: "12h".to_string(),
                effective: "1h".to_string(),
            }]
        );
    }

    #[test]
    fn a_bad_update_check_variable_or_file_value_is_a_usage_error_naming_it() {
        for value in ["soon", "0s", "-1", "0"] {
            let err =
                resolve_with("", &[("GTUNNEL_UPDATE_CHECK_INTERVAL", value)]).expect_err(value);
            let (code, text) = exit_and_text(&err);
            assert_eq!(code, 2, "{value}: {text}");
            assert!(
                text.contains("GTUNNEL_UPDATE_CHECK_INTERVAL"),
                "{value}: {text}"
            );
        }
        let err = resolve_with("update_check_interval = \"0s\"\n", &[]).expect_err("zero");
        let (code, msg) = exit_and_text(&err);
        assert_eq!(code, 2, "{msg}");
        assert!(msg.contains("update_check_interval"), "{msg}");
    }

    #[test]
    fn the_watch_keys_default_to_what_the_constants_were() {
        let s = settings_of("", &[]);
        assert_eq!(s.watch_probe_timeout, Duration::from_secs(3));
        assert_eq!(s.watch_degraded_after, 2);
        assert_eq!(s.watch_interval, Duration::from_secs(30));
    }

    #[test]
    fn the_file_sets_the_watch_keys() {
        let s = settings_of(
            "[watch]\nprobe_timeout = \"5s\"\ndegraded_after = 4\ninterval = \"45s\"\n",
            &[],
        );
        assert_eq!(s.watch_probe_timeout, Duration::from_secs(5));
        assert_eq!(s.watch_degraded_after, 4);
        assert_eq!(s.watch_interval, Duration::from_secs(45));
    }

    #[test]
    fn a_watch_variable_beats_the_same_key_in_the_file() {
        let file = "[watch]\nprobe_timeout = \"5s\"\ndegraded_after = 4\ninterval = \"45s\"\n";
        let r = resolve_with(
            file,
            &[
                ("GTUNNEL_WATCH_PROBE_TIMEOUT", "1s"),
                ("GTUNNEL_WATCH_DEGRADED_AFTER", "3"),
                ("GTUNNEL_WATCH_INTERVAL", "10s"),
            ],
        )
        .expect("resolves");
        assert_eq!(r.settings.watch_probe_timeout, Duration::from_secs(1));
        assert_eq!(r.settings.watch_degraded_after, 3);
        assert_eq!(r.settings.watch_interval, Duration::from_secs(10));
        let seen: Vec<(&str, &str)> = r.overrides.iter().map(|o| (o.key, o.variable)).collect();
        assert_eq!(
            seen,
            vec![
                ("watch.probe_timeout", "GTUNNEL_WATCH_PROBE_TIMEOUT"),
                ("watch.degraded_after", "GTUNNEL_WATCH_DEGRADED_AFTER"),
                ("watch.interval", "GTUNNEL_WATCH_INTERVAL"),
            ]
        );
    }

    #[test]
    fn a_bad_watch_variable_is_a_usage_error_naming_it() {
        for (var, value) in [
            ("GTUNNEL_WATCH_PROBE_TIMEOUT", "soon"),
            ("GTUNNEL_WATCH_PROBE_TIMEOUT", "0s"),
            ("GTUNNEL_WATCH_DEGRADED_AFTER", "many"),
            ("GTUNNEL_WATCH_DEGRADED_AFTER", "0"),
            ("GTUNNEL_WATCH_DEGRADED_AFTER", "-1"),
            ("GTUNNEL_WATCH_INTERVAL", "soon"),
            ("GTUNNEL_WATCH_INTERVAL", "0s"),
            ("GTUNNEL_WATCH_INTERVAL", "1500ms"),
        ] {
            let err = resolve_with("", &[(var, value)]).expect_err(var);
            let (code, text) = exit_and_text(&err);
            assert_eq!(code, 2, "{var}={value}: {text}");
            assert!(text.contains(var), "{var}={value}: {text}");
        }
    }

    #[test]
    fn a_bad_watch_file_value_is_a_usage_error_naming_the_key() {
        for (text, key) in [
            ("[watch]\nprobe_timeout = \"0s\"\n", "watch.probe_timeout"),
            ("[watch]\nprobe_timeout = \"x\"\n", "watch.probe_timeout"),
            ("[watch]\ndegraded_after = 0\n", "watch.degraded_after"),
            ("[watch]\ndegraded_after = -2\n", "watch.degraded_after"),
            ("[watch]\ninterval = \"0s\"\n", "watch.interval"),
            ("[watch]\ninterval = \"2500ms\"\n", "watch.interval"),
        ] {
            let err = resolve_with(text, &[]).expect_err(text);
            let (code, msg) = exit_and_text(&err);
            assert_eq!(code, 2, "{msg}");
            assert!(msg.contains(key), "{msg}");
        }
    }

    #[test]
    fn an_empty_watch_variable_is_unset() {
        let s = settings_of(
            "[watch]\ndegraded_after = 4\n",
            &[("GTUNNEL_WATCH_DEGRADED_AFTER", " ")],
        );
        assert_eq!(s.watch_degraded_after, 4);
    }

    // The TUI trusts the heartbeat for as long as it would take the watchdog to
    // miss three passes, so a slower watchdog must not read as a dead one
    #[test]
    fn the_heartbeat_stays_fresh_for_longer_than_one_interval() {
        for secs in [1, 30, 45, 90, 600] {
            let fresh = heartbeat_fresh_secs(Duration::from_secs(secs));
            assert!(fresh > secs, "{secs}s interval, {fresh}s freshness");
        }
        assert_eq!(heartbeat_fresh_secs(Duration::from_secs(30)), 90);
        assert_eq!(heartbeat_fresh_secs(Duration::from_secs(10)), 90);
        assert_eq!(heartbeat_fresh_secs(Duration::from_secs(60)), 180);
    }

    #[test]
    fn the_logs_keys_default_to_what_the_constants_were() {
        let s = settings_of("", &[]);
        assert_eq!(s.logs_max_size, 8 * 1024 * 1024);
        assert_eq!(s.logs_keep_size, 2 * 1024 * 1024);
        assert_eq!(s.logs_lines_per_minute, 60);
        assert_eq!(s.logs_auth_exit_window, Duration::from_secs(30));
    }

    #[test]
    fn the_file_sets_the_logs_keys() {
        let s = settings_of(
            "[logs]\nmax_size = \"16MiB\"\nkeep_size = \"512KiB\"\nlines_per_minute = 10\nauth_exit_window = \"1m\"\n",
            &[],
        );
        assert_eq!(s.logs_max_size, 16 * 1024 * 1024);
        assert_eq!(s.logs_keep_size, 512 * 1024);
        assert_eq!(s.logs_lines_per_minute, 10);
        assert_eq!(s.logs_auth_exit_window, Duration::from_secs(60));
    }

    #[test]
    fn a_logs_variable_beats_the_same_key_in_the_file() {
        let file = "[logs]\nmax_size = \"16MiB\"\nkeep_size = \"512KiB\"\nlines_per_minute = 10\nauth_exit_window = \"1m\"\n";
        let r = resolve_with(
            file,
            &[
                ("GTUNNEL_LOGS_MAX_SIZE", "4MiB"),
                ("GTUNNEL_LOGS_KEEP_SIZE", "1048576"),
                ("GTUNNEL_LOGS_LINES_PER_MINUTE", "20"),
                ("GTUNNEL_LOGS_AUTH_EXIT_WINDOW", "5s"),
            ],
        )
        .expect("resolves");
        assert_eq!(r.settings.logs_max_size, 4 * 1024 * 1024);
        assert_eq!(r.settings.logs_keep_size, 1024 * 1024);
        assert_eq!(r.settings.logs_lines_per_minute, 20);
        assert_eq!(r.settings.logs_auth_exit_window, Duration::from_secs(5));
        let seen: Vec<(&str, &str)> = r.overrides.iter().map(|o| (o.key, o.variable)).collect();
        assert_eq!(
            seen,
            vec![
                ("logs.max_size", "GTUNNEL_LOGS_MAX_SIZE"),
                ("logs.keep_size", "GTUNNEL_LOGS_KEEP_SIZE"),
                ("logs.lines_per_minute", "GTUNNEL_LOGS_LINES_PER_MINUTE"),
                ("logs.auth_exit_window", "GTUNNEL_LOGS_AUTH_EXIT_WINDOW"),
            ]
        );
    }

    #[test]
    fn a_bad_logs_variable_is_a_usage_error_naming_it() {
        for (var, value) in [
            ("GTUNNEL_LOGS_MAX_SIZE", "huge"),
            ("GTUNNEL_LOGS_MAX_SIZE", "0"),
            ("GTUNNEL_LOGS_MAX_SIZE", "8 parsecs"),
            ("GTUNNEL_LOGS_KEEP_SIZE", "-1MiB"),
            ("GTUNNEL_LOGS_LINES_PER_MINUTE", "lots"),
            ("GTUNNEL_LOGS_LINES_PER_MINUTE", "0"),
            ("GTUNNEL_LOGS_AUTH_EXIT_WINDOW", "soon"),
            ("GTUNNEL_LOGS_AUTH_EXIT_WINDOW", "0s"),
        ] {
            let err = resolve_with("", &[(var, value)]).expect_err(var);
            let (code, text) = exit_and_text(&err);
            assert_eq!(code, 2, "{var}={value}: {text}");
            assert!(text.contains(var), "{var}={value}: {text}");
        }
    }

    #[test]
    fn a_bad_logs_file_value_is_a_usage_error_naming_the_key() {
        for (text, key) in [
            ("[logs]\nmax_size = \"huge\"\n", "logs.max_size"),
            ("[logs]\nkeep_size = \"0\"\n", "logs.keep_size"),
            ("[logs]\nlines_per_minute = 0\n", "logs.lines_per_minute"),
            (
                "[logs]\nauth_exit_window = \"0s\"\n",
                "logs.auth_exit_window",
            ),
        ] {
            let err = resolve_with(text, &[]).expect_err(text);
            let (code, msg) = exit_and_text(&err);
            assert_eq!(code, 2, "{msg}");
            assert!(msg.contains(key), "{msg}");
        }
    }

    #[test]
    fn keep_size_must_be_smaller_than_max_size_and_the_error_names_both_keys() {
        for (text, vars) in [
            ("[logs]\nmax_size = \"1MiB\"\n", vec![]),
            ("[logs]\nmax_size = \"2MiB\"\n", vec![]),
            ("", vec![("GTUNNEL_LOGS_KEEP_SIZE", "9MiB")]),
            ("", vec![("GTUNNEL_LOGS_MAX_SIZE", "1MiB")]),
        ] {
            let err = resolve_with(text, &vars).expect_err(text);
            let (code, msg) = exit_and_text(&err);
            assert_eq!(code, 2, "{msg}");
            assert!(msg.contains("logs.keep_size"), "{msg}");
            assert!(msg.contains("logs.max_size"), "{msg}");
        }
        let err = resolve_with("", &[("GTUNNEL_LOGS_MAX_SIZE", "1MiB")]).expect_err("clash");
        assert!(format!("{err:#}").contains("GTUNNEL_LOGS_MAX_SIZE"));
    }

    #[test]
    fn a_size_reads_bytes_and_binary_units_and_prints_the_largest_whole_one() {
        for (raw, bytes) in [
            ("1048576", 1_048_576),
            ("4096B", 4096),
            ("8MiB", 8 << 20),
            ("8 mib", 8 << 20),
            ("512KiB", 512 << 10),
            ("1GiB", 1 << 30),
        ] {
            assert_eq!(parse_size("k", raw).expect(raw), bytes, "{raw}");
        }
        assert_eq!(show_size(8 << 20), "8MiB");
        assert_eq!(show_size(1536 << 10), "1536KiB");
        assert_eq!(show_size(1000), "1000B");
        assert_eq!(show_size(2 << 30), "2GiB");
    }

    #[test]
    fn the_gcp_keys_default_to_what_the_constants_were() {
        let s = settings_of("", &[]);
        assert_eq!(s.gcp_token_ttl, Duration::from_secs(45 * 60));
        assert_eq!(s.gcp_operation_timeout, Duration::from_secs(600));
        assert_eq!(s.gcp_http_timeout, Duration::from_secs(30));
        assert_eq!(s.gcp_poll_interval, Duration::from_secs(2));
        assert_eq!(s.gcp_lock_deadline, Duration::from_secs(5));
        assert_eq!(s.gcp_fingerprint_attempts, 5);
    }

    #[test]
    fn the_file_sets_the_gcp_keys() {
        let s = settings_of(
            "[gcp]\ntoken_ttl = \"30m\"\noperation_timeout = \"20m\"\nhttp_timeout = \"10s\"\npoll_interval = \"500ms\"\nlock_deadline = \"15s\"\nfingerprint_attempts = 8\n",
            &[],
        );
        assert_eq!(s.gcp_token_ttl, Duration::from_secs(30 * 60));
        assert_eq!(s.gcp_operation_timeout, Duration::from_secs(20 * 60));
        assert_eq!(s.gcp_http_timeout, Duration::from_secs(10));
        assert_eq!(s.gcp_poll_interval, Duration::from_millis(500));
        assert_eq!(s.gcp_lock_deadline, Duration::from_secs(15));
        assert_eq!(s.gcp_fingerprint_attempts, 8);
    }

    #[test]
    fn a_gcp_variable_beats_the_same_key_in_the_file() {
        let file = "[gcp]\ntoken_ttl = \"30m\"\noperation_timeout = \"20m\"\nhttp_timeout = \"10s\"\npoll_interval = \"500ms\"\nlock_deadline = \"15s\"\nfingerprint_attempts = 8\n";
        let r = resolve_with(
            file,
            &[
                ("GTUNNEL_GCP_TOKEN_TTL", "20m"),
                ("GTUNNEL_GCP_OPERATION_TIMEOUT", "5m"),
                ("GTUNNEL_GCP_HTTP_TIMEOUT", "1m"),
                ("GTUNNEL_GCP_POLL_INTERVAL", "1s"),
                ("GTUNNEL_GCP_LOCK_DEADLINE", "2s"),
                ("GTUNNEL_GCP_FINGERPRINT_ATTEMPTS", "3"),
            ],
        )
        .expect("resolves");
        assert_eq!(r.settings.gcp_token_ttl, Duration::from_secs(20 * 60));
        assert_eq!(r.settings.gcp_operation_timeout, Duration::from_secs(300));
        assert_eq!(r.settings.gcp_http_timeout, Duration::from_secs(60));
        assert_eq!(r.settings.gcp_poll_interval, Duration::from_secs(1));
        assert_eq!(r.settings.gcp_lock_deadline, Duration::from_secs(2));
        assert_eq!(r.settings.gcp_fingerprint_attempts, 3);
        let seen: Vec<(&str, &str)> = r.overrides.iter().map(|o| (o.key, o.variable)).collect();
        assert_eq!(
            seen,
            vec![
                ("gcp.token_ttl", "GTUNNEL_GCP_TOKEN_TTL"),
                ("gcp.operation_timeout", "GTUNNEL_GCP_OPERATION_TIMEOUT"),
                ("gcp.http_timeout", "GTUNNEL_GCP_HTTP_TIMEOUT"),
                ("gcp.poll_interval", "GTUNNEL_GCP_POLL_INTERVAL"),
                ("gcp.lock_deadline", "GTUNNEL_GCP_LOCK_DEADLINE"),
                (
                    "gcp.fingerprint_attempts",
                    "GTUNNEL_GCP_FINGERPRINT_ATTEMPTS"
                ),
            ]
        );
    }

    #[test]
    fn a_bad_gcp_variable_is_a_usage_error_naming_it() {
        for (var, value) in [
            ("GTUNNEL_GCP_TOKEN_TTL", "soon"),
            ("GTUNNEL_GCP_TOKEN_TTL", "0s"),
            ("GTUNNEL_GCP_OPERATION_TIMEOUT", "0s"),
            ("GTUNNEL_GCP_HTTP_TIMEOUT", "fast"),
            ("GTUNNEL_GCP_HTTP_TIMEOUT", "0s"),
            ("GTUNNEL_GCP_POLL_INTERVAL", "0s"),
            ("GTUNNEL_GCP_LOCK_DEADLINE", "never"),
            ("GTUNNEL_GCP_LOCK_DEADLINE", "0s"),
            ("GTUNNEL_GCP_FINGERPRINT_ATTEMPTS", "many"),
            ("GTUNNEL_GCP_FINGERPRINT_ATTEMPTS", "0"),
        ] {
            let err = resolve_with("", &[(var, value)]).expect_err(var);
            let (code, text) = exit_and_text(&err);
            assert_eq!(code, 2, "{var}={value}: {text}");
            assert!(text.contains(var), "{var}={value}: {text}");
        }
    }

    #[test]
    fn a_bad_gcp_file_value_is_a_usage_error_naming_the_key() {
        for (text, key) in [
            ("[gcp]\ntoken_ttl = \"0s\"\n", "gcp.token_ttl"),
            (
                "[gcp]\noperation_timeout = \"x\"\n",
                "gcp.operation_timeout",
            ),
            ("[gcp]\nhttp_timeout = \"0s\"\n", "gcp.http_timeout"),
            ("[gcp]\npoll_interval = \"0s\"\n", "gcp.poll_interval"),
            ("[gcp]\nlock_deadline = \"0s\"\n", "gcp.lock_deadline"),
            (
                "[gcp]\nfingerprint_attempts = 0\n",
                "gcp.fingerprint_attempts",
            ),
        ] {
            let err = resolve_with(text, &[]).expect_err(text);
            let (code, msg) = exit_and_text(&err);
            assert_eq!(code, 2, "{msg}");
            assert!(msg.contains(key), "{msg}");
        }
    }
}
