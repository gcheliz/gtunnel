use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::config::Account;

// Where the token comes from. Tests use a fixed string; everything else shells
// out to gcloud so gtunnel never stores a credential of its own.
#[derive(Debug, Clone)]
pub enum TokenSource {
    Gcloud(Box<Account>),
    #[cfg(test)]
    Static(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub compute: String,
    pub dns: String,
    pub certmanager: String,
    pub iap: String,
    pub monitoring: String,
    pub resource_manager: String,
}

impl Endpoints {
    pub fn google() -> Self {
        Self {
            compute: "https://compute.googleapis.com/compute/v1".to_string(),
            dns: "https://dns.googleapis.com/dns/v1".to_string(),
            certmanager: "https://certificatemanager.googleapis.com/v1".to_string(),
            iap: "https://iap.googleapis.com/v1".to_string(),
            monitoring: "https://monitoring.googleapis.com/v3".to_string(),
            resource_manager: "https://cloudresourcemanager.googleapis.com/v3".to_string(),
        }
    }

    // The endpoints a command's client talks to. A debug build lets the
    // environment move every API to one base, so a test can run the real binary
    // against a fake GCP; a release build cannot, and the variable's name is
    // not in it at all. Were it honoured there, whoever controls a process's
    // environment could send the account's access token to a host of their
    // choosing. CI greps the release binary for the name.
    pub fn from_env() -> Self {
        #[cfg(debug_assertions)]
        {
            Self::from_lookup(&|name| std::env::var(name).ok())
        }
        #[cfg(not(debug_assertions))]
        {
            Self::google()
        }
    }

    // The lookup is a parameter so a unit test does not have to mutate the
    // process environment, which every other test in the binary shares.
    #[cfg(debug_assertions)]
    fn from_lookup(env: &dyn Fn(&str) -> Option<String>) -> Self {
        match env("GTUNNEL_TEST_GCP_ENDPOINT") {
            Some(base) if !base.is_empty() => Self::single(&base),
            _ => Self::google(),
        }
    }

    // Point every API at one base, so a mock server can serve them all
    #[cfg(any(test, debug_assertions))]
    pub fn single(base: &str) -> Self {
        Self {
            compute: base.to_string(),
            dns: base.to_string(),
            certmanager: base.to_string(),
            iap: base.to_string(),
            monitoring: base.to_string(),
            resource_manager: base.to_string(),
        }
    }
}

#[derive(Debug)]
pub enum GcpError {
    NotFound,
    PreconditionFailed,
    /// `reason` is the API's own machine word (`SERVICE_DISABLED`,
    /// `RATE_LIMIT_EXCEEDED`): the status alone cannot tell a disabled API from
    /// a missing role, and the two want different advice
    Api {
        status: u16,
        message: String,
        reason: Option<String>,
    },
}

impl std::fmt::Display for GcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GcpError::NotFound => write!(f, "not found (404)"),
            GcpError::PreconditionFailed => {
                write!(f, "precondition failed (412): fingerprint mismatch")
            }
            GcpError::Api {
                status, message, ..
            } => write!(f, "GCP API error {status}: {message}"),
        }
    }
}

impl std::error::Error for GcpError {}

pub fn is_not_found(err: &anyhow::Error) -> bool {
    matches!(err.downcast_ref::<GcpError>(), Some(GcpError::NotFound))
}

pub fn is_precondition_failed(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<GcpError>(),
        Some(GcpError::PreconditionFailed)
    )
}

// A long-running operation, in both the compute and the google.longrunning shapes
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub self_link: Option<String>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
    #[serde(default)]
    pub done: Option<bool>,
}

impl Operation {
    fn is_done(&self) -> bool {
        self.done == Some(true) || self.status.as_deref() == Some("DONE")
    }
}

pub struct GcpClient {
    http: reqwest::Client,
    source: TokenSource,
    pub endpoints: Endpoints,
    cached: Mutex<Option<(String, Instant)>>,
    // Read once per client, from `gcp.*`: a client lives for one command, so
    // a value edited in the file applies to the next one
    token_ttl: Duration,
    poll_interval: Duration,
    operation_timeout: Duration,
}

impl GcpClient {
    pub fn new(source: TokenSource, endpoints: Endpoints) -> Self {
        Self::with_settings(source, endpoints, &crate::settings::current())
    }

    fn with_settings(
        source: TokenSource,
        endpoints: Endpoints,
        settings: &crate::settings::Settings,
    ) -> Self {
        Self {
            http: crate::http::client_builder()
                .timeout(settings.gcp_http_timeout)
                .build()
                .expect("reqwest client"),
            source,
            endpoints,
            cached: Mutex::new(None),
            token_ttl: settings.gcp_token_ttl,
            // A mock server answers at once, and the default sleep would make
            // every operation test wait two seconds for nothing
            #[cfg(not(test))]
            poll_interval: settings.gcp_poll_interval,
            #[cfg(test)]
            poll_interval: Duration::from_millis(10),
            operation_timeout: settings.gcp_operation_timeout,
        }
    }

    pub fn for_account(acct: &Account) -> Self {
        Self::new(
            TokenSource::Gcloud(Box::new(acct.clone())),
            Endpoints::from_env(),
        )
    }

    async fn token(&self) -> Result<String> {
        match &self.source {
            #[cfg(test)]
            TokenSource::Static(t) => Ok(t.clone()),
            TokenSource::Gcloud(acct) => {
                let mut cached = self.cached.lock().await;
                if let Some((token, at)) = cached.as_ref()
                    && at.elapsed() < self.token_ttl
                {
                    return Ok(token.clone());
                }
                let token = super::gcloud::print_access_token(acct).await?;
                *cached = Some((token.clone(), Instant::now()));
                Ok(token)
            }
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<serde_json::Value>,
    ) -> Result<String> {
        let token = self.token().await?;
        let mut req = self.http.request(method.clone(), url).bearer_auth(token);
        if let Some(body) = &body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("{method} {url} failed"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if status.is_success() {
            return Ok(text);
        }
        let message = extract_error_message(&text).unwrap_or_else(|| {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                status.to_string()
            } else {
                trimmed.to_string()
            }
        });
        let err = match status.as_u16() {
            404 => GcpError::NotFound,
            412 => GcpError::PreconditionFailed,
            code => GcpError::Api {
                status: code,
                message,
                reason: extract_error_reason(&text),
            },
        };
        Err(anyhow::Error::new(err).context(format!("{method} {url}")))
    }

    pub async fn get<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let text = self.send(reqwest::Method::GET, url, None).await?;
        parse_json(&text, url)
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, url: &str, body: &B) -> Result<T> {
        let json = serde_json::to_value(body).context("Failed to serialize request body")?;
        let text = self.send(reqwest::Method::POST, url, Some(json)).await?;
        parse_json(&text, url)
    }

    pub async fn patch<B: Serialize, T: DeserializeOwned>(&self, url: &str, body: &B) -> Result<T> {
        let json = serde_json::to_value(body).context("Failed to serialize request body")?;
        let text = self.send(reqwest::Method::PATCH, url, Some(json)).await?;
        parse_json(&text, url)
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, url: &str, body: &B) -> Result<T> {
        let json = serde_json::to_value(body).context("Failed to serialize request body")?;
        let text = self.send(reqwest::Method::PUT, url, Some(json)).await?;
        parse_json(&text, url)
    }

    pub async fn delete(&self, url: &str) -> Result<Option<Operation>> {
        let text = self.send(reqwest::Method::DELETE, url, None).await?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        Ok(serde_json::from_str(&text).ok())
    }

    // Block until a long-running operation finishes, or report why it failed.
    // Compute answers with an absolute selfLink; use wait_operation_at for an
    // API that answers with a bare resource name instead
    pub async fn wait_operation(&self, op: &Operation) -> Result<()> {
        self.poll_operation(op, None).await
    }

    // `base` is the API root the operation came from, used to resolve a bare
    // `name` that google.longrunning APIs return in place of a selfLink
    pub async fn wait_operation_at(&self, op: &Operation, base: &str) -> Result<()> {
        self.poll_operation(op, Some(base)).await
    }

    async fn poll_operation(&self, op: &Operation, base: Option<&str>) -> Result<()> {
        if let Some(err) = &op.error {
            return Err(operation_error(err));
        }
        if op.is_done() {
            return Ok(());
        }
        let url = match (&op.self_link, &op.name) {
            (Some(link), _) => link.clone(),
            // A name is relative to its API root; reqwest rejects it alone
            (None, Some(name)) => {
                let base = base.ok_or_else(|| {
                    anyhow!("Operation '{name}' has no selfLink and no API base to resolve it")
                })?;
                format!(
                    "{}/{}",
                    base.trim_end_matches('/'),
                    name.trim_start_matches('/')
                )
            }
            (None, None) => return Err(anyhow!("Operation has neither selfLink nor name")),
        };

        let started = Instant::now();
        loop {
            tokio::time::sleep(self.poll_interval).await;
            let current: Operation = self.get(&url).await?;
            if let Some(err) = &current.error {
                return Err(operation_error(err));
            }
            if current.is_done() {
                return Ok(());
            }
            if started.elapsed() > self.operation_timeout {
                return Err(anyhow!(
                    "Operation {} did not finish within {} seconds",
                    url,
                    self.operation_timeout.as_secs()
                ));
            }
        }
    }
}

fn parse_json<T: DeserializeOwned>(text: &str, url: &str) -> Result<T> {
    if text.trim().is_empty() {
        // Some deletes answer 200 with no body; let unit types through
        return serde_json::from_str("null").with_context(|| format!("Empty response from {url}"));
    }
    serde_json::from_str(text).with_context(|| {
        let preview: String = text.chars().take(200).collect();
        format!("Failed to parse response from {url}: {preview}")
    })
}

fn extract_error_message(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    value
        .get("error")?
        .get("message")?
        .as_str()
        .map(|s| s.to_string())
}

// The most specific machine word the body carries: Google's newer APIs put it
// in `details[].reason`, Compute and the older ones in `errors[].reason`, and
// `error.status` (`PERMISSION_DENIED`) is the coarse fallback
fn extract_error_reason(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let error = value.get("error")?;
    ["details", "errors"]
        .iter()
        .filter_map(|key| error.get(key)?.as_array())
        .flatten()
        .find_map(|item| item.get("reason")?.as_str())
        .or_else(|| error.get("status")?.as_str())
        .map(str::to_string)
}

fn operation_error(err: &serde_json::Value) -> anyhow::Error {
    let detail = err
        .get("errors")
        .and_then(|e| e.as_array())
        .map(|errors| {
            errors
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|s| !s.is_empty())
        .or_else(|| {
            err.get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| err.to_string());
    anyhow!("Operation failed: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn the_most_specific_reason_in_an_error_body_is_kept() {
        let disabled = r#"{"error":{"code":403,"message":"API disabled","status":"PERMISSION_DENIED",
            "details":[{"@type":"x","reason":"SERVICE_DISABLED"}]}}"#;
        assert_eq!(
            extract_error_reason(disabled).as_deref(),
            Some("SERVICE_DISABLED")
        );
        let compute =
            r#"{"error":{"code":403,"message":"m","errors":[{"reason":"rateLimitExceeded"}]}}"#;
        assert_eq!(
            extract_error_reason(compute).as_deref(),
            Some("rateLimitExceeded")
        );
        let coarse = r#"{"error":{"code":403,"message":"m","status":"PERMISSION_DENIED"}}"#;
        assert_eq!(
            extract_error_reason(coarse).as_deref(),
            Some("PERMISSION_DENIED")
        );
        assert_eq!(extract_error_reason("not json"), None);
        assert_eq!(extract_error_reason(r#"{"error":{"message":"m"}}"#), None);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn an_unset_endpoint_variable_leaves_every_api_at_google() {
        assert_eq!(Endpoints::from_lookup(&|_| None), Endpoints::google());
        // An empty value is a shell that exported the name and nothing else
        assert_eq!(
            Endpoints::from_lookup(&|_| Some(String::new())),
            Endpoints::google()
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    fn a_set_endpoint_variable_points_every_api_at_that_base() {
        let got = Endpoints::from_lookup(&|name| {
            (name == "GTUNNEL_TEST_GCP_ENDPOINT").then(|| "http://127.0.0.1:9".to_string())
        });
        assert_eq!(got, Endpoints::single("http://127.0.0.1:9"));
    }

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    struct Thing {
        name: String,
    }

    fn client(server: &MockServer) -> GcpClient {
        GcpClient::new(
            TokenSource::Static("tok-123".to_string()),
            Endpoints::single(&server.uri()),
        )
    }

    #[tokio::test]
    async fn get_sends_the_bearer_token_and_parses_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/thing"))
            .and(header("authorization", "Bearer tok-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "gt-dev-relay"
            })))
            .mount(&server)
            .await;

        let got: Thing = client(&server)
            .get(&format!("{}/thing", server.uri()))
            .await
            .expect("get");
        assert_eq!(got.name, "gt-dev-relay");
    }

    #[tokio::test]
    async fn a_404_is_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": 404, "message": "not found"}
            })))
            .mount(&server)
            .await;

        let err = client(&server)
            .get::<Thing>(&format!("{}/thing", server.uri()))
            .await
            .expect_err("should fail");
        assert!(is_not_found(&err), "got: {err:?}");
        assert!(!is_precondition_failed(&err), "got: {err:?}");
    }

    // Review Focus 4: concurrent edits of a url map or instance group return 412
    #[tokio::test]
    async fn a_412_is_precondition_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(412).set_body_json(serde_json::json!({
                "error": {"code": 412, "message": "fingerprint mismatch"}
            })))
            .mount(&server)
            .await;

        let err = client(&server)
            .get::<Thing>(&format!("{}/thing", server.uri()))
            .await
            .expect_err("should fail");
        assert!(is_precondition_failed(&err), "got: {err:?}");
    }

    #[tokio::test]
    async fn an_api_error_keeps_its_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {"code": 403, "message": "denied"}
            })))
            .mount(&server)
            .await;

        let err = client(&server)
            .get::<Thing>(&format!("{}/thing", server.uri()))
            .await
            .expect_err("should fail");
        let text = format!("{err:#}");
        assert!(text.contains("denied"), "got: {text}");
        assert!(text.contains("403"), "got: {text}");
    }

    #[tokio::test]
    async fn delete_treats_404_as_already_gone() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let out = client(&server)
            .delete(&format!("{}/thing", server.uri()))
            .await;
        assert!(out.is_err(), "the client surfaces 404; callers decide");
        assert!(is_not_found(&out.unwrap_err()));
    }

    // Certificate Manager and other google.longrunning APIs answer with a bare
    // resource name and no selfLink. Handing that to reqwest as a URL fails
    // with "relative URL without a base", which aborts the first `relay up`
    #[tokio::test]
    async fn wait_operation_resolves_a_bare_operation_name_against_the_api_base() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/locations/global/operations/op-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "projects/p/locations/global/operations/op-1",
                "done": true
            })))
            .mount(&server)
            .await;

        let client = GcpClient::new(
            TokenSource::Static("tok".to_string()),
            Endpoints::single(&server.uri()),
        );
        let op = Operation {
            name: Some("projects/p/locations/global/operations/op-1".to_string()),
            status: None,
            self_link: None,
            error: None,
            done: Some(false),
        };
        client
            .wait_operation_at(&op, &server.uri())
            .await
            .expect("a bare name must be resolved against the base");
    }

    #[tokio::test]
    async fn wait_operation_polls_until_done() {
        let server = MockServer::start().await;
        let op_path = "/projects/p/global/operations/op-1";
        Mock::given(method("GET"))
            .and(path(op_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "op-1", "status": "PENDING"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(op_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "op-1", "status": "DONE"
            })))
            .mount(&server)
            .await;

        let op = Operation {
            name: Some("op-1".to_string()),
            status: Some("PENDING".to_string()),
            self_link: Some(format!("{}{}", server.uri(), op_path)),
            error: None,
            done: None,
        };
        client(&server).wait_operation(&op).await.expect("wait");
        let hits = server.received_requests().await.expect("requests").len();
        assert_eq!(hits, 2, "should poll until DONE");
    }

    #[tokio::test]
    async fn wait_operation_surfaces_the_operation_error() {
        let server = MockServer::start().await;
        let op_path = "/projects/p/global/operations/op-2";
        Mock::given(method("GET"))
            .and(path(op_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "op-2",
                "status": "DONE",
                "error": {"errors": [{"code": "RESOURCE_IN_USE", "message": "still referenced"}]}
            })))
            .mount(&server)
            .await;

        // Starts PENDING, so the error has to come back from the poll
        let op = Operation {
            name: Some("op-2".to_string()),
            status: Some("PENDING".to_string()),
            self_link: Some(format!("{}{}", server.uri(), op_path)),
            error: None,
            done: None,
        };
        let err = client(&server)
            .wait_operation(&op)
            .await
            .expect_err("should fail");
        assert!(
            format!("{err:#}").contains("still referenced"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn wait_operation_accepts_an_already_done_operation() {
        let server = MockServer::start().await;
        let op = Operation {
            name: Some("op-3".to_string()),
            status: Some("DONE".to_string()),
            self_link: None,
            error: None,
            done: Some(true),
        };
        client(&server).wait_operation(&op).await.expect("wait");
        assert!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty(),
            "a finished operation needs no poll"
        );
    }

    #[test]
    fn google_endpoints_point_at_the_real_apis() {
        let e = Endpoints::google();
        assert_eq!(e.compute, "https://compute.googleapis.com/compute/v1");
        assert_eq!(e.dns, "https://dns.googleapis.com/dns/v1");
        assert_eq!(
            e.certmanager,
            "https://certificatemanager.googleapis.com/v1"
        );
        assert_eq!(e.iap, "https://iap.googleapis.com/v1");
        assert_eq!(e.monitoring, "https://monitoring.googleapis.com/v3");
    }
}
