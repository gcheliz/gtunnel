use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::config::Config;
use crate::state::{PersistentTunnel, TunnelSpec};
use crate::tunnel;

pub mod launchd;
pub mod systemd;

// What a service manager needs to run one tunnel
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSpec {
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub log_path: PathBuf,
    pub env: Vec<(String, String)>,
    pub auto_start: bool,
}

// Runtime facts the service manager reports back
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub pid: Option<u32>,
    pub restarts: u32,
}

// A daemon does not inherit an interactive shell's PATH, so gcloud's directory
// is written into the unit. Without it the daemon dies at startup every time.
fn daemon_env(gcloud: &std::path::Path) -> Vec<(String, String)> {
    let mut path = String::new();
    if let Some(dir) = gcloud.parent() {
        path.push_str(&dir.to_string_lossy());
        path.push(':');
    }
    path.push_str("/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin");
    vec![
        ("CLOUDSDK_CORE_DISABLE_PROMPTS".to_string(), "1".to_string()),
        ("PATH".to_string(), path),
    ]
}

pub fn service_spec(cfg: &Config, t: &PersistentTunnel) -> Result<ServiceSpec> {
    let acct = cfg.get_account(Some(&t.account_name))?;
    let gcloud = crate::gcp::gcloud::require_gcloud()?;
    let args = match &t.spec {
        TunnelSpec::Access { .. } => tunnel::access_argv(acct, t)?,
        TunnelSpec::Expose { .. } => {
            let ex = acct.expose.as_ref().with_context(|| {
                format!(
                    "Account '{}' has no relay. Run `gtunnel relay up`",
                    acct.name
                )
            })?;
            tunnel::expose_argv(acct, ex, t)?
        }
    };
    Ok(ServiceSpec {
        label: platform_label(&t.account_name, &t.name),
        program: gcloud.clone(),
        args,
        log_path: t.log_path()?,
        env: daemon_env(&gcloud),
        auto_start: t.auto_start,
    })
}

#[cfg(target_os = "macos")]
pub fn platform_label(account: &str, name: &str) -> String {
    launchd::label_for(account, name)
}

#[cfg(not(target_os = "macos"))]
pub fn platform_label(account: &str, name: &str) -> String {
    systemd::unit_for(account, name)
}

// The platform backend every command calls; consumers land in Task 10
#[allow(unused_imports)]
#[cfg(target_os = "macos")]
pub use launchd::{install, runtime_info, set_auto_start, start, status, stop, uninstall};

#[allow(unused_imports)]
#[cfg(not(target_os = "macos"))]
pub use systemd::{install, runtime_info, set_auto_start, start, status, stop, uninstall};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TunnelStatus;

    fn spec() -> ServiceSpec {
        ServiceSpec {
            label: "com.gtunnel.dev.db".to_string(),
            program: std::path::PathBuf::from("/opt/homebrew/bin/gcloud"),
            args: vec![
                "compute".to_string(),
                "start-iap-tunnel".to_string(),
                "my vm".to_string(),
                "5432".to_string(),
                "--local-host-port=localhost:15432".to_string(),
            ],
            log_path: std::path::PathBuf::from("/tmp/gtunnel/logs/dev-db.log"),
            env: vec![
                ("CLOUDSDK_CORE_DISABLE_PROMPTS".to_string(), "1".to_string()),
                (
                    "PATH".to_string(),
                    "/opt/homebrew/bin:/usr/bin:/bin".to_string(),
                ),
            ],
            auto_start: false,
        }
    }

    // Review Focus 1: a daemon that cannot find gcloud must fail loudly, not
    // silently look healthy, so PATH and the restart policy are part of the unit
    #[test]
    fn plist_carries_label_args_path_and_restart_policy() {
        let plist = launchd::render_plist(&spec());
        assert!(
            plist.contains("<string>com.gtunnel.dev.db</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>/opt/homebrew/bin/gcloud</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<string>start-iap-tunnel</string>"),
            "{plist}"
        );
        assert!(plist.contains("<string>my vm</string>"), "{plist}");
        assert!(plist.contains("<key>PATH</key>"), "{plist}");
        assert!(
            plist.contains("<string>/opt/homebrew/bin:/usr/bin:/bin</string>"),
            "{plist}"
        );
        assert!(
            plist.contains("<key>CLOUDSDK_CORE_DISABLE_PROMPTS</key>"),
            "{plist}"
        );
        assert!(plist.contains("<key>SuccessfulExit</key>"), "{plist}");
        assert!(plist.contains("<false/>"), "{plist}");
        assert!(plist.contains("<key>ThrottleInterval</key>"), "{plist}");
        assert!(
            plist.contains("<string>/tmp/gtunnel/logs/dev-db.log</string>"),
            "{plist}"
        );
    }

    // RunAtLoad no longer carries auto-start: KeepAlive overrides it, and the
    // plist is loaded only when the tunnel is meant to run
    #[test]
    fn a_loaded_plist_always_runs() {
        let mut s = spec();
        assert!(launchd::render_plist(&s).contains("<key>RunAtLoad</key>\n    <true/>"));
        s.auto_start = true;
        assert!(launchd::render_plist(&s).contains("<key>RunAtLoad</key>\n    <true/>"));
    }

    // KeepAlive{SuccessfulExit:false} treats a SIGTERM death as a failure, so
    // `launchctl kill` only bought ~10s before launchd brought the tunnel back
    // while `gtunnel stop` had already printed stopped
    #[test]
    fn stopping_unloads_the_job_so_keepalive_cannot_respawn_it() {
        assert_eq!(
            launchd::stop_args("gui/501", "com.gtunnel.dev.db"),
            vec!["bootout", "gui/501/com.gtunnel.dev.db"]
        );
    }

    #[test]
    fn starting_rebootstraps_a_job_that_stop_unloaded() {
        let (load, run) = launchd::start_args(
            "gui/501",
            "com.gtunnel.dev.db",
            "/Users/me/Library/LaunchAgents/com.gtunnel.dev.db.plist",
        );
        assert_eq!(
            load,
            vec![
                "bootstrap",
                "gui/501",
                "/Users/me/Library/LaunchAgents/com.gtunnel.dev.db.plist"
            ]
        );
        assert_eq!(run, vec!["kickstart", "gui/501/com.gtunnel.dev.db"]);
    }

    // With KeepAlive set, launchd runs a job the moment it is loaded, whatever
    // RunAtLoad or Disabled say, and it loads everything in LaunchAgents at
    // login. So a plist kept there is a tunnel that always runs: the loaded
    // definition gtunnel starts and stops lives in its own directory, and the
    // LaunchAgents copy exists only when auto-start is on.
    #[test]
    fn only_an_auto_start_tunnel_puts_a_plist_in_launch_agents() {
        let canonical = launchd::plist_path("com.gtunnel.dev.db").expect("path");
        assert!(
            !canonical.starts_with(launchd::agents_dir().expect("agents")),
            "{}",
            canonical.display()
        );
        let agent = launchd::agent_path("com.gtunnel.dev.db").expect("path");
        assert!(
            agent.starts_with(launchd::agents_dir().expect("agents")),
            "{}",
            agent.display()
        );
        assert_eq!(canonical.file_name(), agent.file_name());
    }

    #[test]
    fn plist_escapes_xml_in_values() {
        let mut s = spec();
        s.args.push("a&b<c>".to_string());
        let plist = launchd::render_plist(&s);
        assert!(
            plist.contains("<string>a&amp;b&lt;c&gt;</string>"),
            "{plist}"
        );
        assert!(!plist.contains("a&b<c>"), "{plist}");
    }

    #[test]
    fn unit_quotes_arguments_and_sets_environment() {
        let unit = systemd::render_unit(&spec());
        assert!(
            unit.contains(
                "ExecStart=/opt/homebrew/bin/gcloud compute start-iap-tunnel \"my vm\" 5432"
            ),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=CLOUDSDK_CORE_DISABLE_PROMPTS=1"),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=PATH=/opt/homebrew/bin:/usr/bin:/bin"),
            "{unit}"
        );
        assert!(unit.contains("Restart=on-failure"), "{unit}");
        assert!(unit.contains("RestartSec=5"), "{unit}");
        assert!(
            unit.contains("StandardOutput=append:/tmp/gtunnel/logs/dev-db.log"),
            "{unit}"
        );
        assert!(
            unit.contains("StandardError=append:/tmp/gtunnel/logs/dev-db.log"),
            "{unit}"
        );
        assert!(unit.contains("WantedBy=default.target"), "{unit}");
    }

    #[test]
    fn launchctl_print_reports_running_crashed_and_stopped() {
        let running = r#"
	state = running
	pid = 4242
	runs = 1
"#;
        let (status, info) = launchd::parse_launchctl_print(running);
        assert_eq!(status, TunnelStatus::Running);
        assert_eq!(info.pid, Some(4242));
        assert_eq!(info.restarts, 0);

        let crashed = r#"
	state = not running
	runs = 4
	last exit code = 1
"#;
        let (status, info) = launchd::parse_launchctl_print(crashed);
        assert_eq!(status, TunnelStatus::Error);
        assert_eq!(info.pid, None);
        assert_eq!(info.restarts, 3);

        let stopped = r#"
	state = not running
	runs = 2
	last exit code = 0
"#;
        let (status, _) = launchd::parse_launchctl_print(stopped);
        assert_eq!(status, TunnelStatus::Stopped);

        let never_run = "	state = not running\n";
        let (status, info) = launchd::parse_launchctl_print(never_run);
        assert_eq!(status, TunnelStatus::Stopped);
        assert_eq!(info.restarts, 0);
    }

    // Real `launchctl print` nests endpoint blocks whose own "state = active"
    // lines must not be mistaken for the job's state
    #[test]
    fn launchctl_print_ignores_nested_endpoint_state() {
        let real = "com.gtunnel.dev.db = {\n\tstate = running\n\truns = 1\n\tpid = 96316\n\tlast exit code = (never exited)\n\tendpoints = {\n\t\t\"x\" = {\n\t\t\tstate = active\n\t\t}\n\t}\n}\n";
        let (status, info) = launchd::parse_launchctl_print(real);
        assert_eq!(status, TunnelStatus::Running);
        assert_eq!(info.pid, Some(96316));
    }

    #[test]
    fn systemctl_show_reports_active_failed_and_inactive() {
        let active = "ActiveState=active\nMainPID=4242\nNRestarts=2\n";
        let (status, info) = systemd::parse_systemctl_show(active);
        assert_eq!(status, TunnelStatus::Running);
        assert_eq!(info.pid, Some(4242));
        assert_eq!(info.restarts, 2);

        let failed = "ActiveState=failed\nMainPID=0\nNRestarts=5\n";
        let (status, info) = systemd::parse_systemctl_show(failed);
        assert_eq!(status, TunnelStatus::Error);
        assert_eq!(info.pid, None);
        assert_eq!(info.restarts, 5);

        let inactive = "ActiveState=inactive\nMainPID=0\nNRestarts=0\n";
        let (status, _) = systemd::parse_systemctl_show(inactive);
        assert_eq!(status, TunnelStatus::Stopped);
    }

    // The joined "<account>-<name>" label cannot be split back apart
    // ("prod-eu-api" is ambiguous), so both parts stay separate
    #[test]
    fn service_labels_follow_the_platform_convention() {
        assert_eq!(launchd::label_for("dev", "db"), "com.gtunnel.dev.db");
        assert_eq!(
            launchd::label_for("prod-eu", "api"),
            "com.gtunnel.prod-eu.api"
        );
        assert_eq!(systemd::unit_for("dev", "db"), "gtunnel-dev-db.service");
        assert_eq!(
            systemd::unit_for("prod-eu", "api"),
            "gtunnel-prod-eu-api.service"
        );
    }
}

// Live launchd lifecycle check, run by hand on macOS:
//   cargo test --features _live_daemon daemon::live -- --ignored --nocapture
#[cfg(all(test, target_os = "macos"))]
mod live {
    use super::*;
    use crate::state::TunnelStatus;

    #[tokio::test]
    #[ignore = "touches the real launchd domain"]
    async fn install_start_status_stop_uninstall() {
        let _temp = crate::config::testing::TempConfigDir::new();
        let dir = _temp.path();
        let spec = ServiceSpec {
            label: "com.gtunnel.test.sleep".to_string(),
            program: std::path::PathBuf::from("/bin/sleep"),
            args: vec!["600".to_string()],
            log_path: dir.join("logs/test-sleep.log"),
            env: vec![("PATH".to_string(), "/usr/bin:/bin".to_string())],
            auto_start: false,
        };

        launchd::install(&spec).await.expect("install");
        println!(
            "plist written: {}",
            std::fs::read_to_string(
                dirs::home_dir()
                    .unwrap()
                    .join("Library/LaunchAgents/com.gtunnel.test.sleep.plist")
            )
            .unwrap_or_else(|e| format!("READ ERROR {e}"))
        );
        let raw = tokio::process::Command::new("launchctl")
            .args([
                "print",
                &format!("gui/{}/com.gtunnel.test.sleep", unsafe { libc::getuid() }),
            ])
            .output()
            .await
            .unwrap();
        println!(
            "print rc={:?} stderr={}",
            raw.status.code(),
            String::from_utf8_lossy(&raw.stderr)
        );
        launchd::start(&spec.label).await.expect("start");
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let running = launchd::status(&spec.label).await;
        let info = launchd::runtime_info(&spec.label).await;
        println!("after start: status={running:?} info={info:?}");
        assert_eq!(running, TunnelStatus::Running);
        assert!(info.pid.is_some(), "a running job should report a pid");

        launchd::stop(&spec.label).await.expect("stop");
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        println!(
            "after stop: status={:?}",
            launchd::status(&spec.label).await
        );

        launchd::uninstall(&spec.label).await.expect("uninstall");
        assert_eq!(
            launchd::status(&spec.label).await,
            TunnelStatus::Stopped,
            "an uninstalled job should not be running"
        );
        println!("after uninstall: gone");
    }
}
