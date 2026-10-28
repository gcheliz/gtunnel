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

pub fn default_discovery() -> bool {
    true
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
    pub discovery: bool,
    pub notifications: bool,
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
    let discovery = switch_key(env, &DISCOVERY_KEY, config.discovery, &mut overrides)?;
    let notifications = switch_key(
        env,
        &NOTIFICATIONS_KEY,
        config.notifications.enabled,
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
            discovery,
            notifications,
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
        assert!(s.notifications);
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
        assert!(s.notifications);
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
                ("GTUNNEL_NOTIFICATIONS", "off"),
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
}
