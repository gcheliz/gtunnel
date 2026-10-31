pub mod access;
pub mod account;
pub mod completions;
pub mod expose;
pub mod maint;
pub mod relocate;
pub mod resume;
pub mod supervise;
pub mod watch;

use anyhow::{Context, Result};

use crate::error::GtunnelError;
use std::io::Write;

use crate::config::{load_config, Account, Config};
use crate::daemon;
use crate::state::{PersistentTunnel, TunnelState, TunnelStatus};

// The narrowest each column is allowed to be. A column grows to fit its
// widest value and never shrinks below this, so a listing of short names
// looks the way it always has and a listing of long ones still lines up.
const NAME_FLOOR: usize = 16;
const MODE_WIDTH: usize = 7;
const URL_FLOOR: usize = 32;

pub fn status_word(status: TunnelStatus) -> &'static str {
    match status {
        TunnelStatus::Running => "running",
        TunnelStatus::Stopped => "stopped",
        TunnelStatus::Error => "error",
    }
}

/// `[run, stops at 14:32]`, or `[run]` for a run with no TTL. Local time, since
/// the reader compares it with a clock on the wall, and a TTL is at most a day
/// so the hour and minute cannot be mistaken for another day's.
pub fn run_note(expires_at: Option<u64>) -> String {
    match expires_at.and_then(local_hhmm) {
        Some(time) => format!("  [run, stops at {time}]"),
        None => "  [run]".to_string(),
    }
}

pub(crate) fn local_hhmm(unix_secs: u64) -> Option<String> {
    let t = libc::time_t::try_from(unix_secs).ok()?;
    // SAFETY: localtime_r writes into the zeroed tm we own and reads the
    // time_t we pass by reference; neither pointer outlives the call, and unlike
    // localtime it keeps no shared static buffer, so threads cannot race on it.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        tm
    };
    Some(format!("{:02}:{:02}", tm.tm_hour, tm.tm_min))
}

/// One line of `gtunnel list`, before it knows how wide its columns are.
///
/// Rows are collected before any of them is printed because a column cannot
/// be sized until the longest value in it is known — and the unmanaged
/// tunnels, which are discovered last, have the longest names of all.
pub struct ListRow {
    pub status: TunnelStatus,
    pub name: String,
    pub mode: &'static str,
    pub url: String,
    pub target: String,
    /// Appended after the target, for a tunnel gtunnel does not own.
    pub note: Option<String>,
}

impl ListRow {
    /// A live `gtunnel run`. The mode column keeps saying what the tunnel is,
    /// so a reader scanning it still finds every public one; that it ends by
    /// itself is a trailing marker, like the one on a tunnel gtunnel does not
    /// own, and says when so nobody has to do the arithmetic.
    pub fn from_run(record: &crate::ephemeral::EphemeralRecord) -> Self {
        let t = record.as_tunnel();
        Self {
            status: TunnelStatus::Running,
            name: t.name.clone(),
            mode: t.display_kind(),
            url: t.display_url(),
            target: t.display_target(),
            note: Some(run_note(record.expires_at())),
        }
    }

    pub fn from_tunnel(status: TunnelStatus, t: &PersistentTunnel) -> Self {
        Self {
            status,
            name: t.name.clone(),
            mode: t.display_kind(),
            url: t.display_url(),
            target: t.display_target(),
            note: None,
        }
    }

    /// A stopped tunnel whose supervisor gave up on expired credentials. The
    /// reason rides in the trailing note, like a run's, so the status column
    /// keeps the one word `stopped` that scripts and the layout already rely on.
    pub fn stopped_for_credentials(mut self, marked: Option<crate::failure::Failure>) -> Self {
        if let Some(failure) = marked {
            self.note = Some(
                if failure == crate::failure::Failure::LoginExpired {
                    "  [auth expired]"
                } else {
                    "  [credentials refused]"
                }
                .to_string(),
            );
        }
        self
    }

    /// A stopped tunnel whose supervisor spent its restart budget, with the
    /// last thing gcloud died of. Same trailing note as the credential case,
    /// so `stopped` stays the one status word.
    pub fn stopped_for_budget(mut self, reason: Option<&str>) -> Self {
        if let Some(reason) = reason {
            self.note = Some(format!("  [restart budget spent: {reason}]"));
        }
        self
    }

    pub fn render(&self, layout: &ListLayout) -> String {
        let note = self.note.as_deref().unwrap_or_default();
        format!(
            "{} {:<8} {:<name$} {:<MODE_WIDTH$} {:<url$} {}{note}",
            self.status.symbol(),
            status_word(self.status),
            self.name,
            self.mode,
            self.url,
            self.target,
            name = layout.name,
            url = layout.url,
        )
        .trim_end()
        .to_string()
    }
}

/// How wide each variable column has to be for this particular listing.
pub struct ListLayout {
    name: usize,
    url: usize,
}

impl ListLayout {
    /// Cells, not bytes: a name or a URL may hold a multi-byte character, and
    /// `len()` would reserve too many columns for it.
    pub fn fit(rows: &[ListRow]) -> Self {
        let widest = |f: fn(&ListRow) -> &str, floor: usize| {
            rows.iter()
                .map(|r| f(r).chars().count())
                .max()
                .unwrap_or(0)
                .max(floor)
        };
        Self {
            name: widest(|r| &r.name, NAME_FLOOR),
            url: widest(|r| &r.url, URL_FLOOR),
        }
    }

    pub fn header(&self) -> String {
        format!(
            "{:<10} {:<name$} {:<MODE_WIDTH$} {:<url$} {}",
            "STATUS",
            "NAME",
            "MODE",
            "URL",
            "TARGET",
            name = self.name,
            url = self.url,
        )
    }
}

// Read one line, falling back to the default when the user just presses enter
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub fn prompt(label: &str, default: Option<&str>) -> Result<String> {
    match default {
        Some(d) => print!("{label} [{d}]: "),
        None => print!("{label}: "),
    }
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("Failed to read from stdin")?;
    let value = line.trim().to_string();
    if value.is_empty() {
        return match default {
            Some(d) => Ok(d.to_string()),
            None => Ok(String::new()),
        };
    }
    Ok(value)
}

/// The zone a command acts in: the flag, else the account's default. An account
/// saved without a zone is legitimate (`init` lets it be skipped), so the
/// refusal belongs here, where a zone is consumed, and runs before any GCP
/// request so a missing zone never reaches the API as an empty path segment.
pub fn require_zone(acct: &Account, flag: Option<&str>) -> Result<String> {
    if let Some(zone) = flag {
        crate::state::validate_zone(zone)?;
        return Ok(zone.to_string());
    }
    if acct.default_zone.is_empty() {
        return Err(GtunnelError::usage(format!(
            "no zone for account '{}': pass --zone, or set a default with \
             gtunnel account edit {} --zone <zone>",
            acct.name, acct.name
        ))
        .into());
    }
    Ok(acct.default_zone.clone())
}

/// A zone that could not be settled by itself. Its own type, not a bare
/// `GtunnelError`, because a person at a terminal or in the TUI can answer it:
/// the candidates and the prefill are what the zone prompt shows. The kind
/// keeps the exit code the issue fixes for a script (not found 3, ambiguous 2).
#[derive(Debug)]
pub struct ZoneUnresolved {
    pub kind: crate::error::ErrorKind,
    message: String,
    /// The zones that hold an instance of that name, when several do
    pub candidates: Vec<String>,
    /// What the prompt starts with: the account default, or empty
    pub prefill: String,
}

impl std::fmt::Display for ZoneUnresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ZoneUnresolved {}

/// Where an existing instance lives. `verified` says the lookup itself found
/// it, so the separate existence check would only repeat the question.
pub struct InstanceZone {
    pub zone: String,
    pub verified: bool,
    /// Set when the lookup could not run and the account default stood in
    pub warning: Option<String>,
}

/// The zone of an instance the command names. `can_ask` says someone is there
/// to answer a prompt (a terminal, the TUI): a failed lookup then comes back as
/// `ZoneUnresolved` prefilled with the default, where a script gets the default
/// with a warning. `--zone` wins; otherwise the
/// project is asked, because a name identifies the instance and the zone is a
/// fact to read rather than a question to put to the person. `require_zone`
/// stays for callers with nothing to look up (the relay creates its VM).
pub async fn resolve_instance_zone(
    client: &crate::gcp::GcpClient,
    acct: &Account,
    flag: Option<&str>,
    instance: &str,
    can_ask: bool,
) -> Result<InstanceZone> {
    use crate::error::ErrorKind;
    if let Some(zone) = flag {
        crate::state::validate_zone(zone)?;
        return Ok(InstanceZone {
            zone: zone.to_string(),
            verified: false,
            warning: None,
        });
    }
    let unresolved = |kind, message: String, candidates| ZoneUnresolved {
        kind,
        message,
        candidates,
        prefill: acct.default_zone.clone(),
    };
    match crate::gcp::compute::find_instances(client, &acct.project, instance).await {
        Ok(found) => match found.as_slice() {
            [one] => Ok(InstanceZone {
                zone: one.zone.clone(),
                verified: true,
                warning: None,
            }),
            [] => Err(unresolved(
                ErrorKind::NotFound,
                format!(
                    "Instance '{instance}' not found in project '{}'. Check the name, or \
                     pass --zone if it lives in another project's zone list",
                    acct.project
                ),
                Vec::new(),
            )
            .into()),
            many => {
                let zones: Vec<String> = many.iter().map(|i| i.zone.clone()).collect();
                Err(unresolved(
                    ErrorKind::Usage,
                    format!(
                        "Instance '{instance}' exists in several zones of project '{}' ({}); \
                         pass --zone to choose one",
                        acct.project,
                        zones.join(", ")
                    ),
                    zones,
                )
                .into())
            }
        },
        // Someone who can answer is asked, with the default as the prefill:
        // a lookup that failed is not a reason to pick their zone for them
        Err(e) if !acct.default_zone.is_empty() && can_ask => Err(unresolved(
            ErrorKind::Usage,
            format!("could not look up the zone of instance '{instance}': {e:#}"),
            Vec::new(),
        )
        .into()),
        Err(e) if !acct.default_zone.is_empty() => Ok(InstanceZone {
            zone: acct.default_zone.clone(),
            verified: false,
            warning: Some(format!(
                "could not look up the zone of '{instance}' ({e:#}); \
                 using the account's default zone '{}'",
                acct.default_zone
            )),
        }),
        Err(e) => {
            let mut err = unresolved(
                ErrorKind::Usage,
                format!(
                    "could not look up the zone of instance '{instance}': {e:#}. \
                     Pass --zone"
                ),
                Vec::new(),
            );
            // A lookup refused for credentials keeps its own exit code
            err.kind = crate::error::kind_of(&e);
            if err.kind == ErrorKind::Internal {
                err.kind = ErrorKind::Usage;
            }
            Err(err.into())
        }
    }
}

/// Ask for the zone a failed resolution could not settle. Several matches are
/// listed and picked by number or name, as an account's zones are; the
/// prefill is what Enter keeps.
#[allow(clippy::print_stdout, clippy::print_stderr)]
pub fn prompt_zone(why: &ZoneUnresolved) -> Result<String> {
    eprintln!("{why}");
    for (index, z) in why.candidates.iter().enumerate() {
        println!("  {}. {z}", index + 1);
    }
    let prefill = (!why.prefill.is_empty()).then_some(why.prefill.as_str());
    let answer = prompt("Zone", prefill)?;
    let zone = account::resolve_zone_choice(&answer, &why.candidates);
    if zone.is_empty() {
        return Err(GtunnelError::new(why.kind, why.to_string()).into());
    }
    crate::state::validate_zone(&zone)?;
    Ok(zone)
}

/// `resolve_instance_zone`, then the prompt when a terminal is there to
/// answer it. A script keeps the typed error and its exit code.
pub async fn resolve_instance_zone_or_ask(
    client: &crate::gcp::GcpClient,
    acct: &Account,
    flag: Option<&str>,
    instance: &str,
) -> Result<InstanceZone> {
    let can_ask = crate::gcp::gcloud::is_interactive();
    match resolve_instance_zone(client, acct, flag, instance, can_ask).await {
        Err(e) if can_ask => match e.downcast_ref::<ZoneUnresolved>() {
            Some(why) => Ok(InstanceZone {
                zone: prompt_zone(why)?,
                verified: false,
                warning: None,
            }),
            None => Err(e),
        },
        other => other,
    }
}

pub fn confirm(question: &str, assume_yes: bool) -> Result<bool> {
    if assume_yes {
        return Ok(true);
    }
    let answer = prompt(&format!("{question} [y/N]"), Some("n"))?;
    Ok(matches!(answer.to_lowercase().as_str(), "y" | "yes"))
}

// The config plus the account this invocation acts on
pub fn resolve(account: Option<&str>) -> Result<(Config, Account)> {
    let config = load_config()?;
    let acct = config.get_account(account)?.clone();
    Ok((config, acct))
}

// Find a tunnel of this account by name, with a message that names the account
pub fn find_tunnel(state: &TunnelState, name: &str, account: &str) -> Result<PersistentTunnel> {
    state
        .find_for_account(name, account)
        .cloned()
        .ok_or_else(|| {
            GtunnelError::not_found(format!("Tunnel '{name}' not found for account '{account}'"))
                .into()
        })
}

pub async fn tunnel_status(t: &PersistentTunnel) -> TunnelStatus {
    daemon::status(&daemon::platform_label(&t.account_name, &t.name)).await
}

#[cfg(test)]
mod tests {

    // The zone is a fact the project holds, so an add that names only the
    // instance must read it rather than ask for it
    mod zone_lookup {
        use super::super::*;
        use crate::error::{kind_of, ErrorKind};
        use crate::gcp::GcpClient;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        fn acct(default_zone: &str) -> Account {
            Account {
                name: "dev".to_string(),
                gcloud_configuration: "dev".to_string(),
                project: "example-dev-1a2b".to_string(),
                default_zone: default_zone.to_string(),
                impersonate_service_account: None,
                credential_file: None,
                expose: None,
            }
        }

        async fn answering(zones: &[&str]) -> MockServer {
            let server = MockServer::start().await;
            let items: serde_json::Map<String, serde_json::Value> = zones
                .iter()
                .map(|z| {
                    (
                        format!("zones/{z}"),
                        serde_json::json!({"instances": [{"name": "bastion",
                            "zone": format!("https://x/projects/p/zones/{z}")}]}),
                    )
                })
                .collect();
            Mock::given(method("GET"))
                .and(path("/projects/example-dev-1a2b/aggregated/instances"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({ "items": items })),
                )
                .mount(&server)
                .await;
            server
        }

        #[tokio::test]
        async fn one_match_gives_its_zone_with_no_flag_and_no_default() {
            let server = answering(&["europe-west4-a"]).await;
            let got = resolve_instance_zone(
                &GcpClient::for_mock(&server),
                &acct(""),
                None,
                "bastion",
                false,
            )
            .await
            .expect("resolved");
            assert_eq!(got.zone, "europe-west4-a");
            assert!(got.verified, "the lookup already proved it exists");
        }

        #[tokio::test]
        async fn the_flag_is_used_without_asking_the_api() {
            let server = MockServer::start().await;
            let got = resolve_instance_zone(
                &GcpClient::for_mock(&server),
                &acct("europe-west1-b"),
                Some("us-central1-a"),
                "bastion",
                false,
            )
            .await
            .expect("resolved");
            assert_eq!(got.zone, "us-central1-a");
            assert!(!got.verified);
            let seen = server.received_requests().await.unwrap_or_default();
            assert!(seen.is_empty(), "{} requests", seen.len());
        }

        #[tokio::test]
        async fn two_zones_are_a_usage_error_naming_both() {
            let server = answering(&["us-central1-a", "europe-west1-b"]).await;
            let err = resolve_instance_zone(
                &GcpClient::for_mock(&server),
                &acct(""),
                None,
                "bastion",
                false,
            )
            .await
            .err()
            .expect("ambiguous");
            assert_eq!(kind_of(&err).exit_code(), 2);
            let msg = format!("{err:#}");
            assert!(
                msg.contains("europe-west1-b") && msg.contains("us-central1-a"),
                "{msg}"
            );
            assert!(msg.contains("--zone"), "{msg}");
            let why = err.downcast_ref::<ZoneUnresolved>().expect("typed");
            assert_eq!(why.candidates, vec!["europe-west1-b", "us-central1-a"]);
        }

        #[tokio::test]
        async fn no_such_instance_is_not_found_and_names_the_project() {
            let server = answering(&[]).await;
            let err = resolve_instance_zone(
                &GcpClient::for_mock(&server),
                &acct("europe-west1-b"),
                None,
                "bastion",
                false,
            )
            .await
            .err()
            .expect("not found");
            assert_eq!(kind_of(&err), ErrorKind::NotFound);
            assert_eq!(kind_of(&err).exit_code(), 3);
            assert!(format!("{err:#}").contains("example-dev-1a2b"));
            let why = err.downcast_ref::<ZoneUnresolved>().expect("typed");
            assert_eq!(
                why.prefill, "europe-west1-b",
                "the prompt offers the default"
            );
        }

        #[tokio::test]
        async fn a_lookup_that_fails_falls_back_to_the_default_zone() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
            let got = resolve_instance_zone(
                &GcpClient::for_mock(&server),
                &acct("europe-west1-b"),
                None,
                "bastion",
                false,
            )
            .await
            .expect("falls back");
            assert_eq!(got.zone, "europe-west1-b");
            assert!(!got.verified, "the fallback still has to be checked");
            assert!(got.warning.is_some());
        }

        // Someone who can answer is asked, with the default to hand, rather
        // than given a zone they never chose
        #[tokio::test]
        async fn a_lookup_that_fails_asks_a_person_prefilled_with_the_default() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
            let err = resolve_instance_zone(
                &GcpClient::for_mock(&server),
                &acct("europe-west1-b"),
                None,
                "bastion",
                true,
            )
            .await
            .err()
            .expect("asks");
            let why = err.downcast_ref::<ZoneUnresolved>().expect("typed");
            assert_eq!(why.prefill, "europe-west1-b");
            assert!(why.candidates.is_empty());
            assert!(format!("{err:#}").contains("could not look up"), "{err:#}");
        }

        #[tokio::test]
        async fn a_lookup_that_fails_with_no_default_says_to_pass_the_zone() {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
            let err = resolve_instance_zone(
                &GcpClient::for_mock(&server),
                &acct(""),
                None,
                "bastion",
                false,
            )
            .await
            .err()
            .expect("no zone");
            assert!(format!("{err:#}").contains("Pass --zone"), "{err:#}");
            assert!(err.downcast_ref::<ZoneUnresolved>().is_some());
        }
    }
    use super::*;
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};

    fn access() -> PersistentTunnel {
        PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: true,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "bastion".to_string(),
                    zone: "europe-west1-b".to_string(),
                },
                remote_port: 5432,
                local_port: 15432,
            },
        }
    }

    fn expose() -> PersistentTunnel {
        PersistentTunnel {
            name: "api".to_string(),
            account_name: "dev".to_string(),
            auto_start: true,
            spec: TunnelSpec::Expose {
                local_target: "3000".to_string(),
                domain: "dev.example.com".to_string(),
                hostname: "api.dev.example.com".to_string(),
                relay_port: 30001,
                private: false,
            },
        }
    }

    fn named(name: &str) -> PersistentTunnel {
        PersistentTunnel {
            name: name.to_string(),
            ..access()
        }
    }

    // Where a column starts, counted in cells from the start of the line.
    fn column_starts(line: &str) -> Vec<usize> {
        let mut starts = Vec::new();
        let mut in_gap = true;
        for (i, c) in line.chars().enumerate() {
            if c == ' ' {
                in_gap = true;
            } else {
                if in_gap {
                    starts.push(i);
                }
                in_gap = false;
            }
        }
        starts
    }

    fn zoneless(zone: &str) -> Account {
        Account {
            name: "work".to_string(),
            gcloud_configuration: "work".to_string(),
            project: "example-dev-1a2b".to_string(),
            default_zone: zone.to_string(),
            impersonate_service_account: None,
            credential_file: None,
            expose: None,
        }
    }

    #[test]
    fn require_zone_refuses_an_empty_default() {
        let err = require_zone(&zoneless(""), None).expect_err("no zone anywhere");
        assert_eq!(crate::error::kind_of(&err), crate::error::ErrorKind::Usage);
        let msg = err.to_string();
        assert!(msg.contains("no zone for account 'work'"), "{msg}");
        assert!(msg.contains("--zone"), "{msg}");
    }

    #[test]
    fn require_zone_prefers_the_flag() {
        let zone = require_zone(&zoneless(""), Some("europe-west1-b")).expect("the flag wins");
        assert_eq!(zone, "europe-west1-b");
        let zone = require_zone(&zoneless("europe-west4-a"), Some("europe-west1-b"))
            .expect("the flag beats the default");
        assert_eq!(zone, "europe-west1-b");
    }

    #[test]
    fn require_zone_uses_the_default() {
        let zone = require_zone(&zoneless("europe-west1-b"), None).expect("the default");
        assert_eq!(zone, "europe-west1-b");
    }

    #[test]
    fn require_zone_still_validates_a_flag() {
        require_zone(&zoneless(""), Some("Not A Zone")).expect_err("a bad flag is refused");
    }

    #[test]
    fn a_run_note_names_the_stop_time_only_with_a_ttl() {
        assert_eq!(run_note(None), "  [run]");
        let with = run_note(Some(1_700_000_000));
        let time = with
            .strip_prefix("  [run, stops at ")
            .and_then(|t| t.strip_suffix(']'))
            .expect(&with);
        assert_eq!(time.chars().count(), 5, "{time}");
        assert_eq!(time.as_bytes()[2], b':');
    }

    #[test]
    fn list_row_holds_status_name_mode_url_and_target() {
        let rows = vec![ListRow::from_tunnel(TunnelStatus::Running, &access())];
        let row = rows[0].render(&ListLayout::fit(&rows));
        let columns: Vec<&str> = row.split_whitespace().collect();
        assert_eq!(columns[0], TunnelStatus::Running.symbol());
        assert_eq!(columns[1], "running");
        assert_eq!(columns[2], "db");
        assert_eq!(columns[3], "access");
        assert_eq!(columns[4], "localhost:15432");
        assert_eq!(columns[5], "bastion:5432");
    }

    #[test]
    fn a_stopped_row_with_a_credentials_marker_says_auth_expired() {
        let plain = vec![ListRow::from_tunnel(TunnelStatus::Stopped, &access())];
        let marked = vec![ListRow::from_tunnel(TunnelStatus::Stopped, &access())
            .stopped_for_credentials(Some(crate::failure::Failure::LoginExpired))];
        let before = plain[0].render(&ListLayout::fit(&plain));
        let after = marked[0].render(&ListLayout::fit(&marked));
        assert!(!before.contains("auth expired"), "{before}");
        assert!(after.contains("auth expired"), "{after}");
        assert!(after.starts_with(&before), "{after}");
    }

    #[test]
    fn a_stopped_row_whose_impersonation_was_denied_says_credentials_refused() {
        let marked = vec![ListRow::from_tunnel(TunnelStatus::Stopped, &access())
            .stopped_for_credentials(Some(crate::failure::Failure::ImpersonationDenied))];
        let text = marked[0].render(&ListLayout::fit(&marked));
        assert!(text.contains("credentials refused"), "{text}");
        assert!(!text.contains("auth expired"), "{text}");
    }

    #[test]
    fn a_stopped_row_with_a_crash_marker_says_restart_budget_spent_and_why() {
        let plain = vec![ListRow::from_tunnel(TunnelStatus::Stopped, &access())];
        let marked = vec![ListRow::from_tunnel(TunnelStatus::Stopped, &access())
            .stopped_for_budget(Some("exit status: 1"))];
        let before = plain[0].render(&ListLayout::fit(&plain));
        let after = marked[0].render(&ListLayout::fit(&marked));
        assert!(
            after.ends_with("[restart budget spent: exit status: 1]"),
            "{after}"
        );
        assert!(after.starts_with(&before), "{after}");
        let none = ListRow::from_tunnel(TunnelStatus::Stopped, &access()).stopped_for_budget(None);
        assert_eq!(none.render(&ListLayout::fit(&plain)), before);
    }

    #[test]
    fn list_row_marks_an_expose_tunnel() {
        let rows = vec![ListRow::from_tunnel(TunnelStatus::Stopped, &expose())];
        let row = rows[0].render(&ListLayout::fit(&rows));
        assert!(row.contains("public"), "{row}");
        assert!(row.contains("https://api.dev.example.com"), "{row}");
    }

    #[test]
    fn the_list_header_lines_up_with_a_row() {
        let rows = vec![ListRow::from_tunnel(TunnelStatus::Running, &access())];
        let layout = ListLayout::fit(&rows);
        let header = layout.header();
        assert_eq!(
            header.split_whitespace().count(),
            5,
            "header columns: {header}"
        );
        // MODE and URL start in the same cell in both lines
        let h = column_starts(&header);
        let r = column_starts(&rows[0].render(&layout));
        assert_eq!((h[2], h[3]), (r[3], r[4]), "header: {header}");
    }

    // A name longer than the floor used to run into the next column, which is
    // what a listing of per-instance tunnels is made of: `redis-realtime-prod`
    // and `mongo-activitylog-prod` are both past sixteen cells.
    #[test]
    fn a_long_name_widens_the_column_instead_of_shunting_the_next_one() {
        let rows = vec![
            ListRow::from_tunnel(TunnelStatus::Running, &named("db")),
            ListRow::from_tunnel(TunnelStatus::Stopped, &named("mongo-activitylog-prod")),
        ];
        let layout = ListLayout::fit(&rows);
        let header = layout.header();
        let starts: Vec<Vec<usize>> = rows
            .iter()
            .map(|r| column_starts(&r.render(&layout)))
            .collect();
        assert_eq!(
            starts[0], starts[1],
            "every row puts its columns in the same cells"
        );
        assert_eq!(
            (column_starts(&header)[2], column_starts(&header)[3]),
            (starts[0][3], starts[0][4]),
            "the header follows the rows"
        );
        // and a short listing is unchanged: the floor still applies
        let short = vec![ListRow::from_tunnel(TunnelStatus::Running, &named("db"))];
        assert_eq!(ListLayout::fit(&short).header(), {
            let l = ListLayout {
                name: NAME_FLOOR,
                url: URL_FLOOR,
            };
            l.header()
        });
    }
}
