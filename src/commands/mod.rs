pub mod access;
pub mod account;
pub mod completions;
pub mod expose;
pub mod maint;
pub mod relocate;
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
    pub fn stopped_for_credentials(mut self, marked: bool) -> Self {
        if marked {
            self.note = Some("  [auth expired]".to_string());
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
    use super::*;
    use crate::state::{AccessTarget, PersistentTunnel, TunnelSpec, TunnelStatus};

    fn access() -> PersistentTunnel {
        PersistentTunnel {
            name: "db".to_string(),
            account_name: "dev".to_string(),
            auto_start: true,
            spec: TunnelSpec::Access {
                target: AccessTarget::Instance {
                    instance: "my-vm".to_string(),
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
        assert_eq!(columns[5], "my-vm:5432");
    }

    #[test]
    fn a_stopped_row_with_a_credentials_marker_says_auth_expired() {
        let plain = vec![ListRow::from_tunnel(TunnelStatus::Stopped, &access())];
        let marked =
            vec![ListRow::from_tunnel(TunnelStatus::Stopped, &access())
                .stopped_for_credentials(true)];
        let before = plain[0].render(&ListLayout::fit(&plain));
        let after = marked[0].render(&ListLayout::fit(&marked));
        assert!(!before.contains("auth expired"), "{before}");
        assert!(after.contains("auth expired"), "{after}");
        assert!(after.starts_with(&before), "{after}");
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
