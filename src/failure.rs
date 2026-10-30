// What a failure is, decided in one place. Before this module three lists of
// substrings (the probe's, the log hints', the refusal check's), a blanket
// 401/403 mapping and a string conversion that dropped the timeout type each
// decided separately, and every one of them defaulted to "your login
// expired". A script then re-authenticated when it should have backed off, and
// a credential-file account was told to log in as a person. The enum below is
// the only vocabulary: producers build it from what they hold (a status, a
// typed error, a stderr line) and every reader (exit code, advice, supervisor,
// health) asks it.

use crate::config::Account;
use crate::error::ErrorKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    LoginExpired,
    ConfigurationMissing,
    ImpersonationDenied,
    KeyInvalid,
    PermissionDenied,
    QuotaOrRateLimited,
    ServiceDisabled,
    NotFound,
    Conflict,
    Network,
    Timeout,
    Other,
}

/// Every needle a log or stderr line is read against, and the only one. A row
/// matches when all its needles are in the line, spelled as the tool spells
/// them: matching without case would read IAP's own `[4004: 'reauthentication
/// required']`, which gcloud resumes by itself, as an expired login. The first
/// row wins, so the narrow
/// causes sit above the broad ones they would otherwise read as: an
/// `invalid_grant` that says `Invalid JWT` is a bad key, not a stale login.
///
/// `[4003:` and `[4033:` are not here on purpose: IAP refusing a port or an
/// identity that logged in fine is not a credential problem, and logging in
/// again changes nothing.
const LINES: &[(Failure, &[&str])] = &[
    (Failure::KeyInvalid, &["Invalid JWT"]),
    (Failure::KeyInvalid, &["Could not deserialize key data"]),
    (Failure::ImpersonationDenied, &["Failed to impersonate"]),
    (
        Failure::ImpersonationDenied,
        &["iam.serviceAccounts.getAccessToken"],
    ),
    (
        Failure::ConfigurationMissing,
        &["configuration [", "does not exist"],
    ),
    (Failure::LoginExpired, &["Reauthentication required"]),
    (Failure::LoginExpired, &["Reauthentication failed"]),
    (Failure::LoginExpired, &["gcloud auth login"]),
    (Failure::LoginExpired, &["invalid_grant"]),
    (Failure::ServiceDisabled, &["SERVICE_DISABLED"]),
    (Failure::ServiceDisabled, &["has not been used in project"]),
    (Failure::ServiceDisabled, &["accessNotConfigured"]),
    (Failure::QuotaOrRateLimited, &["RATE_LIMIT_EXCEEDED"]),
    (Failure::QuotaOrRateLimited, &["RESOURCE_EXHAUSTED"]),
    (Failure::QuotaOrRateLimited, &["Quota exceeded"]),
    (Failure::Network, &["Could not resolve host"]),
    (Failure::Network, &["Network is unreachable"]),
    (Failure::Network, &["Temporary failure in name resolution"]),
    (Failure::Network, &["Name or service not known"]),
    (Failure::Network, &["No route to host"]),
    // macOS words a failed lookup this way, where glibc says "Name or service"
    (Failure::Network, &["nodename nor servname"]),
    (Failure::Network, &["Failed to establish a new connection"]),
    (Failure::Timeout, &["timed out"]),
];

/// The failures a restart cannot fix. A supervisor that sees one stops the
/// job instead of burning its budget on a login or a key that will be refused
/// again, and leaves its slug behind so `list` can say which.
const STOPPING: [Failure; 4] = [
    Failure::LoginExpired,
    Failure::ImpersonationDenied,
    Failure::KeyInvalid,
    Failure::ConfigurationMissing,
];

impl Failure {
    /// What a gcloud stderr or log line says, when it says anything gtunnel
    /// can act on. `None` is the honest answer for the rest: a line nobody
    /// classified must not be guessed into a login problem.
    pub fn from_line(line: &str) -> Option<Failure> {
        LINES
            .iter()
            .find(|(_, needles)| needles.iter().all(|n| line.contains(n)))
            .map(|(failure, _)| *failure)
    }

    /// An API answer. The reason outranks the status because the status is too
    /// coarse: a 403 is a missing role, a disabled API or an exhausted quota,
    /// and each wants a different next step. A 403 with no reason stays
    /// `PermissionDenied`, which is what every 403 was before.
    pub fn from_gcp(status: u16, reason: Option<&str>) -> Failure {
        match reason.map(str::to_lowercase).as_deref() {
            Some("service_disabled" | "accessNotConfigured") => return Failure::ServiceDisabled,
            Some(
                "rate_limit_exceeded"
                | "resource_exhausted"
                | "ratelimitexceeded"
                | "userratelimitexceeded"
                | "quotaexceeded"
                | "dailylimitexceeded",
            ) => return Failure::QuotaOrRateLimited,
            _ => {}
        }
        match status {
            401 => Failure::LoginExpired,
            403 => Failure::PermissionDenied,
            404 => Failure::NotFound,
            408 => Failure::Timeout,
            409 | 412 => Failure::Conflict,
            429 => Failure::QuotaOrRateLimited,
            502..=504 => Failure::Network,
            _ => Failure::Other,
        }
    }

    /// The first cause in the chain that says what went wrong, outermost
    /// first, so a caller that knew better and wrapped the error wins.
    pub fn of(err: &anyhow::Error) -> Option<Failure> {
        err.chain().find_map(Self::of_cause)
    }

    /// One link of a chain. Typed causes are read by type, never by their
    /// message: `proc::Timeout` and `reqwest::Error` were being re-labelled
    /// `auth` by a string conversion, which is how a hung gcloud became an
    /// expired login.
    pub fn of_cause(cause: &(dyn std::error::Error + 'static)) -> Option<Failure> {
        use crate::gcp::client::GcpError;
        if cause.downcast_ref::<crate::proc::Timeout>().is_some() {
            return Some(Failure::Timeout);
        }
        if let Some(e) = cause.downcast_ref::<reqwest::Error>() {
            return if e.is_timeout() {
                Some(Failure::Timeout)
            } else if e.is_connect() {
                Some(Failure::Network)
            } else {
                None
            };
        }
        if let Some(e) = cause.downcast_ref::<GcpError>() {
            return Some(match e {
                GcpError::NotFound => Failure::NotFound,
                GcpError::PreconditionFailed => Failure::Conflict,
                GcpError::Api { status, reason, .. } => {
                    Failure::from_gcp(*status, reason.as_deref())
                }
            });
        }
        let typed = cause.downcast_ref::<crate::error::GtunnelError>()?;
        if let Some(failure) = typed.failure() {
            return Some(failure);
        }
        // Only gcloud's own stderr is read for words: any other message is
        // gtunnel's prose, which names `gcloud auth login` as advice
        if typed.kind == ErrorKind::Gcloud {
            return typed.to_string().lines().find_map(Failure::from_line);
        }
        None
    }

    pub fn kind(self) -> ErrorKind {
        match self {
            Failure::LoginExpired
            | Failure::ImpersonationDenied
            | Failure::KeyInvalid
            | Failure::PermissionDenied => ErrorKind::Auth,
            Failure::ConfigurationMissing | Failure::ServiceDisabled => ErrorKind::Usage,
            Failure::QuotaOrRateLimited | Failure::Network | Failure::Timeout => {
                ErrorKind::Unavailable
            }
            Failure::NotFound => ErrorKind::NotFound,
            Failure::Conflict | Failure::Other => ErrorKind::Internal,
        }
    }

    pub fn is_stopping(self) -> bool {
        STOPPING.contains(&self)
    }

    /// What a supervisor writes into the auth marker.
    pub fn slug(self) -> &'static str {
        match self {
            Failure::LoginExpired => "login-expired",
            Failure::ConfigurationMissing => "configuration-missing",
            Failure::ImpersonationDenied => "impersonation-denied",
            Failure::KeyInvalid => "key-invalid",
            Failure::PermissionDenied => "permission-denied",
            Failure::QuotaOrRateLimited => "quota-or-rate-limited",
            Failure::ServiceDisabled => "service-disabled",
            Failure::NotFound => "not-found",
            Failure::Conflict => "conflict",
            Failure::Network => "network",
            Failure::Timeout => "timeout",
            Failure::Other => "other",
        }
    }

    /// The marker's text back to a failure. Markers written before slugs
    /// existed hold `1`, and they all meant an expired login, so that is what
    /// an unreadable one is too: a marker that exists always means stopped.
    pub fn from_marker(text: &str) -> Failure {
        let text = text.trim();
        STOPPING
            .into_iter()
            .find(|f| f.slug() == text)
            .unwrap_or(Failure::LoginExpired)
    }

    /// One line of what to do next. Only `LoginExpired` names a login, and a
    /// credential-file account does not even get that: a login would sign in
    /// a person, never the key.
    pub fn advice(self, acct: &Account, interactive: bool) -> String {
        let cfg = &acct.gcloud_configuration;
        let project = &acct.project;
        let file = acct
            .credential_file
            .as_ref()
            .map(|f| f.display().to_string());
        match self {
            Failure::LoginExpired => match &file {
                Some(file) => crate::gcp::gcloud::credential_file_advice(file),
                None if interactive => {
                    format!("Run `gcloud auth login --configuration={cfg}` and try again")
                }
                None => format!(
                    "No terminal to log in from. On a workstation run `gcloud auth login \
                     --configuration={cfg}`; on a CI runner, give the runner a service account \
                     with roles/iap.tunnelResourceAccessor, or set credential_file on the account"
                ),
            },
            Failure::ConfigurationMissing => format!(
                "The gcloud configuration `{cfg}` does not exist. List the real ones with \
                 `gcloud config configurations list` and correct gcloud_configuration on \
                 account '{}'",
                acct.name
            ),
            Failure::ImpersonationDenied => format!(
                "The signed-in identity may not impersonate {}. Grant it \
                 roles/iam.serviceAccountTokenCreator on that service account, or remove \
                 impersonate_service_account from account '{}'",
                acct.impersonate_service_account
                    .as_deref()
                    .unwrap_or("the service account"),
                acct.name
            ),
            Failure::KeyInvalid => match &file {
                Some(file) => crate::gcp::gcloud::credential_file_advice(file),
                None => "gcloud rejected the service account key it holds. Replace the key or \
                         activate a valid one with `gcloud auth activate-service-account`"
                    .to_string(),
            },
            Failure::PermissionDenied => format!(
                "The account lacks a permission this call needs in project {project}. Check its \
                 IAM roles (run: gtunnel doctor)"
            ),
            Failure::QuotaOrRateLimited => format!(
                "A quota or rate limit was hit in project {project}. Wait a minute and retry; if \
                 it repeats, request more quota"
            ),
            Failure::ServiceDisabled => format!(
                "A required API is disabled in project {project}. Enable the one the error \
                 names: gcloud services enable <api> --project={project}"
            ),
            Failure::NotFound => {
                "The resource does not exist. Check the name, project and zone".to_string()
            }
            Failure::Conflict => "The resource changed under the request. Retry".to_string(),
            Failure::Network => {
                "Google could not be reached. Check the network connection and retry".to_string()
            }
            Failure::Timeout => {
                "gcloud did not answer in time. Retry; if it repeats, run: gtunnel doctor"
                    .to_string()
            }
            Failure::Other => "Run: gtunnel doctor, or read the message above".to_string(),
        }
    }

    /// The hint a tunnel's log line earns, for the failures that stop a
    /// supervisor. It takes the one fact the log pane has about the account,
    /// whether it signs in with a key, because a hint to log in is wrong for it.
    pub fn log_hint(self, credential_file: bool) -> Option<String> {
        Some(
            match self {
                Failure::LoginExpired if credential_file => {
                    "The credential file gave no token. Check that it exists, is a service-account \
                     key or a workload identity federation file, and that its identity is not \
                     disabled"
                }
                Failure::LoginExpired => "gcloud credentials expired. Run: gcloud auth login",
                Failure::ImpersonationDenied => {
                    "Impersonation was denied. Grant roles/iam.serviceAccountTokenCreator on the \
                     service account, or remove impersonate_service_account from the account"
                }
                Failure::KeyInvalid => {
                    "gcloud rejected the service account key. Replace the key (or the \
                     credential_file) and start the tunnel again"
                }
                Failure::ConfigurationMissing => {
                    "The gcloud configuration named by the account does not exist. Run: gcloud \
                     config configurations list"
                }
                _ => return None,
            }
            .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(credential_file: bool) -> Account {
        Account {
            name: "dev".to_string(),
            gcloud_configuration: "work".to_string(),
            project: "example-dev-1a2b".to_string(),
            default_zone: "europe-west1-b".to_string(),
            impersonate_service_account: Some(
                "runner@example-dev-1a2b.iam.gserviceaccount.com".to_string(),
            ),
            credential_file: credential_file.then(|| "/keys/runner.json".into()),
            expose: None,
        }
    }

    const ALL: [Failure; 12] = [
        Failure::LoginExpired,
        Failure::ConfigurationMissing,
        Failure::ImpersonationDenied,
        Failure::KeyInvalid,
        Failure::PermissionDenied,
        Failure::QuotaOrRateLimited,
        Failure::ServiceDisabled,
        Failure::NotFound,
        Failure::Conflict,
        Failure::Network,
        Failure::Timeout,
        Failure::Other,
    ];

    #[test]
    fn each_sample_gcloud_line_names_its_failure() {
        let cases = [
            (
                "ERROR: (gcloud.auth.print-access-token) There was a problem refreshing your \
                 current auth tokens: Reauthentication required.",
                Some(Failure::LoginExpired),
            ),
            ("Please run: gcloud auth login", Some(Failure::LoginExpired)),
            (
                "invalid_grant: Token has been expired or revoked.",
                Some(Failure::LoginExpired),
            ),
            (
                "ERROR: (gcloud.auth.print-access-token) Failed to impersonate \
                 [runner@example-dev-1a2b.iam.gserviceaccount.com]. Make sure the account that \
                 is trying to impersonate it has access to the service account itself and the \
                 \"roles/iam.serviceAccountTokenCreator\" role.",
                Some(Failure::ImpersonationDenied),
            ),
            (
                "ERROR: (gcloud.auth.print-access-token) The configuration [work] does not exist.",
                Some(Failure::ConfigurationMissing),
            ),
            (
                "invalid_grant: Invalid JWT Signature.",
                Some(Failure::KeyInvalid),
            ),
            (
                "curl: (6) Could not resolve host: oauth2.googleapis.com",
                Some(Failure::Network),
            ),
            (
                "ssh: connect to host 10.0.0.10 port 22: Operation timed out",
                Some(Failure::Timeout),
            ),
            (
                "Error 403: Compute Engine API has not been used in project example-dev-1a2b \
                 before or it is disabled. SERVICE_DISABLED",
                Some(Failure::ServiceDisabled),
            ),
            (
                "Quota exceeded for quota metric 'Read requests' RATE_LIMIT_EXCEEDED",
                Some(Failure::QuotaOrRateLimited),
            ),
            // Lines that are not a token or API failure stay unclassified: a
            // refused identity is IAP's word, not a login problem
            ("Testing if tunnel connection works.", None),
            (
                "INFO: [3] Received WebSocket Close message [4004: 'reauthentication required'].",
                None,
            ),
            ("ERROR: [4033: not authorized]", None),
            ("ERROR: [4003: failed to connect to backend]", None),
            ("", None),
        ];
        for (line, want) in cases {
            assert_eq!(Failure::from_line(line), want, "{line}");
        }
    }

    #[test]
    fn a_gcp_status_and_reason_name_the_failure() {
        let cases = [
            (401, None, Failure::LoginExpired),
            (403, None, Failure::PermissionDenied),
            (403, Some("SERVICE_DISABLED"), Failure::ServiceDisabled),
            (
                403,
                Some("RATE_LIMIT_EXCEEDED"),
                Failure::QuotaOrRateLimited,
            ),
            (429, None, Failure::QuotaOrRateLimited),
            (429, Some("RESOURCE_EXHAUSTED"), Failure::QuotaOrRateLimited),
            (404, None, Failure::NotFound),
            (409, None, Failure::Conflict),
            (503, None, Failure::Network),
            (500, None, Failure::Other),
        ];
        for (status, reason, want) in cases {
            assert_eq!(
                Failure::from_gcp(status, reason),
                want,
                "{status} {reason:?}"
            );
        }
    }

    #[test]
    fn each_failure_maps_to_the_kind_the_plan_names() {
        use ErrorKind::*;
        let cases = [
            (Failure::LoginExpired, Auth),
            (Failure::ImpersonationDenied, Auth),
            (Failure::KeyInvalid, Auth),
            (Failure::PermissionDenied, Auth),
            (Failure::ConfigurationMissing, Usage),
            (Failure::ServiceDisabled, Usage),
            (Failure::QuotaOrRateLimited, Unavailable),
            (Failure::Network, Unavailable),
            (Failure::Timeout, Unavailable),
            (Failure::NotFound, NotFound),
            (Failure::Conflict, Internal),
            (Failure::Other, Internal),
        ];
        for (failure, kind) in cases {
            assert_eq!(failure.kind(), kind, "{failure:?}");
        }
    }

    // The real thing, not a stand-in: the type is private to `proc`, and the
    // point is that `output_within`'s own error is what gets recognised
    #[tokio::test]
    async fn a_timeout_in_the_chain_is_a_timeout_whatever_wraps_it() {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("5");
        let err = crate::proc::output_within(cmd, std::time::Duration::from_millis(20), "gcloud")
            .await
            .unwrap_err();
        let err = err.context("Failed to run gcloud");
        assert_eq!(Failure::of(&err), Some(Failure::Timeout));
    }

    #[test]
    fn a_gcp_error_in_the_chain_is_read_by_its_status_and_reason() {
        use crate::gcp::client::GcpError;
        let err = anyhow::Error::new(GcpError::Api {
            status: 403,
            message: "API disabled".into(),
            reason: Some("SERVICE_DISABLED".into()),
        })
        .context("GET https://example.invalid/x");
        assert_eq!(Failure::of(&err), Some(Failure::ServiceDisabled));
        assert_eq!(
            Failure::of(&anyhow::Error::new(GcpError::NotFound)),
            Some(Failure::NotFound)
        );
        assert_eq!(Failure::of(&anyhow::anyhow!("disk full")), None);
    }

    #[test]
    fn a_gcloud_failure_is_read_from_its_stderr() {
        let err: anyhow::Error = crate::error::GtunnelError::gcloud(
            "gcloud auth print-access-token failed: ERROR: Failed to impersonate [x].",
        )
        .into();
        assert_eq!(Failure::of(&err), Some(Failure::ImpersonationDenied));
    }

    #[test]
    fn a_credential_file_account_is_never_told_to_log_in() {
        for failure in ALL {
            for interactive in [true, false] {
                let advice = failure.advice(&account(true), interactive);
                assert!(!advice.is_empty(), "{failure:?} has advice");
                assert!(!advice.contains("auth login"), "{failure:?}: {advice}");
            }
        }
    }

    #[test]
    fn a_disabled_api_is_advised_by_enabling_it() {
        let advice = Failure::ServiceDisabled.advice(&account(false), true);
        assert!(advice.contains("gcloud services enable"), "{advice}");
        assert!(advice.contains("example-dev-1a2b"), "{advice}");
        assert!(!advice.contains("auth login"), "{advice}");
    }

    #[test]
    fn only_a_login_failure_advises_a_login() {
        for failure in ALL {
            let advice = failure.advice(&account(false), true);
            assert_eq!(
                advice.contains("gcloud auth login"),
                failure == Failure::LoginExpired,
                "{failure:?}: {advice}"
            );
        }
    }

    #[test]
    fn a_marker_slug_round_trips_and_an_old_marker_is_a_login() {
        for failure in [
            Failure::LoginExpired,
            Failure::ImpersonationDenied,
            Failure::KeyInvalid,
            Failure::ConfigurationMissing,
        ] {
            assert!(failure.is_stopping());
            let text = format!("{}\n", failure.slug());
            assert_eq!(Failure::from_marker(&text), failure);
        }
        assert_eq!(Failure::from_marker("1\n"), Failure::LoginExpired);
        assert_eq!(Failure::from_marker(""), Failure::LoginExpired);
        assert!(!Failure::Network.is_stopping());
        assert!(!Failure::PermissionDenied.is_stopping());
    }

    // macOS names a failed lookup differently from glibc, and the supervisor
    // can only ride out an outage it recognises
    #[test]
    fn a_failed_dns_lookup_on_macos_and_a_refused_connection_are_network() {
        for line in [
            "socket.gaierror: [Errno 8] nodename nor servname provided, or not known",
            "urllib3.exceptions.NewConnectionError: Failed to establish a new connection",
        ] {
            assert_eq!(Failure::from_line(line), Some(Failure::Network), "{line}");
        }
    }
}
