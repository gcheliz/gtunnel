use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// A token and how long it has left, when gcloud said.
#[derive(Debug, Clone)]
pub struct IssuedToken {
    pub token: String,
    pub valid_for: Option<Duration>,
}

use crate::config::Account;

// Where the token comes from. Tests use a fixed string; everything else shells
// out to gcloud so gtunnel never stores a credential of its own.
#[derive(Debug, Clone)]
pub enum TokenSource {
    Gcloud(Box<Account>),
    #[cfg(test)]
    Static(String),
    /// Hands out one token per fetch, so a test can tell which request carried
    /// which and how many times the client went back for another
    #[cfg(test)]
    Sequence(std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<IssuedToken>>>),
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

// Fifty pages of the server's default size is far past any project gtunnel
// is pointed at, and a bound is what turns a runaway token into an error
const MAX_PAGES: usize = 50;

// A page token is opaque and may carry `+`, `/` or `=`, so it cannot be
// pasted into a query string as it is
pub(crate) fn encode_query_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
                status,
                message,
                reason,
            } => {
                write!(f, "GCP API error {status}: {message}")?;
                // Google's message names the API and the project but not the
                // command, and "permission denied" is where people look for a
                // role that is not missing
                if crate::failure::Failure::from_gcp(*status, reason.as_deref())
                    == crate::failure::Failure::ServiceDisabled
                {
                    write!(
                        f,
                        ". Enable the API the message names: gcloud services enable <api> \
                         --project=<project>"
                    )?;
                }
                Ok(())
            }
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

struct Reply {
    status: reqwest::StatusCode,
    retry_after: Option<Duration>,
    text: String,
}

// A token held for reuse. It is reusable while it is younger than the
// `gcp.token_ttl` cap and, when gcloud said when it dies, until a margin
// before that: a request that starts with two minutes left can be refused
// halfway through a long command.
struct HeldToken {
    token: String,
    fetched: Instant,
    valid_for: Option<Duration>,
}

// Five minutes covers the longest single call (`gcp.http_timeout`) plus the
// poll interval, with room, so a token is never handed out to expire in flight
const EXPIRY_MARGIN: Duration = Duration::from_secs(5 * 60);

impl HeldToken {
    fn usable(&self, ttl: Duration) -> bool {
        let age = self.fetched.elapsed();
        if age >= ttl {
            return false;
        }
        match self.valid_for {
            Some(left) => age + EXPIRY_MARGIN < left,
            None => true,
        }
    }
}

// The statuses Google's client guidance says to retry with backoff
fn is_transient(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

pub struct GcpClient {
    http: reqwest::Client,
    source: TokenSource,
    pub endpoints: Endpoints,
    cached: Mutex<Option<HeldToken>>,
    // Read once per client, from `gcp.*`: a client lives for one command, so
    // a value edited in the file applies to the next one. `token_ttl` caps
    // how long a token is reused; the token's own expiry, when gcloud
    // reports it, can only shorten that.
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
            _ => {
                let mut cached = self.cached.lock().await;
                if let Some(held) = cached.as_ref()
                    && held.usable(self.token_ttl)
                {
                    return Ok(held.token.clone());
                }
                let issued = self.fetch_token().await?;
                let held = HeldToken {
                    token: issued.token.clone(),
                    fetched: Instant::now(),
                    valid_for: issued.valid_for,
                };
                *cached = Some(held);
                Ok(issued.token)
            }
        }
    }

    async fn fetch_token(&self) -> Result<IssuedToken> {
        match &self.source {
            TokenSource::Gcloud(acct) => super::gcloud::access_token(acct).await,
            #[cfg(test)]
            TokenSource::Static(t) => Ok(IssuedToken {
                token: t.clone(),
                valid_for: None,
            }),
            #[cfg(test)]
            TokenSource::Sequence(queue) => Ok(queue
                .lock()
                .expect("token queue")
                .pop_front()
                .expect("a token queued")),
        }
    }

    // A 401 means the token we hold is not the one the API wants, whatever
    // the clock said, so the next request must not reuse it
    async fn evict_token(&self, rejected: &str) {
        let mut cached = self.cached.lock().await;
        // Another request may already have replaced it; only drop the one
        // that was refused
        if cached.as_ref().is_some_and(|held| held.token == rejected) {
            *cached = None;
        }
    }

    // One attempt: the request and the whole body. A body that fails to
    // arrive is a transport error like any other, not an empty answer, which
    // is how a cut DELETE response used to read as "deleted".
    async fn attempt(
        &self,
        method: &reqwest::Method,
        url: &str,
        token: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Reply, reqwest::Error> {
        let mut req = self.http.request(method.clone(), url).bearer_auth(token);
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(super::retry::parse_retry_after);
        let text = resp.text().await?;
        Ok(Reply {
            status,
            retry_after,
            text,
        })
    }

    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<serde_json::Value>,
    ) -> Result<String> {
        // Repeating these cannot do more than doing them once. A POST or a
        // PATCH may have been applied before the answer was lost, so only a
        // failure to connect, which means nothing was sent, is repeated.
        let idempotent = matches!(
            method,
            reqwest::Method::GET | reqwest::Method::DELETE | reqwest::Method::HEAD
        );
        let mut token = self.token().await?;
        let mut refreshed = false;
        let mut attempt = 1;
        let reply = loop {
            match self.attempt(&method, url, &token, body.as_ref()).await {
                Err(e) => {
                    if attempt < super::retry::ATTEMPTS && (idempotent || e.is_connect()) {
                        tokio::time::sleep(super::retry::delay(attempt, None)).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(anyhow::Error::new(e).context(format!("{method} {url} failed")));
                }
                Ok(reply) if reply.status.as_u16() == 401 && !refreshed => {
                    refreshed = true;
                    self.evict_token(&token).await;
                    token = self.token().await?;
                }
                Ok(reply)
                    if idempotent
                        && attempt < super::retry::ATTEMPTS
                        && is_transient(reply.status.as_u16()) =>
                {
                    tokio::time::sleep(super::retry::delay(attempt, reply.retry_after)).await;
                    attempt += 1;
                }
                Ok(reply) => break reply,
            }
        };
        let Reply { status, text, .. } = reply;

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

    /// Every item of a list call, across pages. A list answers one page at the
    /// size the server picks, so a caller that reads the first page alone sees
    /// an entry on page two as absent and acts on that.
    pub async fn get_all<T: DeserializeOwned>(&self, url: &str, items_key: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        for page in self.pages(url).await? {
            if let Some(items) = page.get(items_key) {
                let items: Vec<T> = serde_json::from_value(items.clone()).with_context(|| {
                    format!("Unexpected '{items_key}' in the response from {url}")
                })?;
                out.extend(items);
            }
        }
        Ok(out)
    }

    /// The same walk for an `aggregatedList`, whose `items` is an object keyed
    /// by scope (`zones/europe-west1-b`) and not an array. A scope that
    /// repeats on a later page is returned again: the caller sees every bucket.
    pub async fn get_all_scoped<T: DeserializeOwned>(
        &self,
        url: &str,
        items_key: &str,
    ) -> Result<Vec<(String, T)>> {
        let mut out = Vec::new();
        for page in self.pages(url).await? {
            if let Some(serde_json::Value::Object(scopes)) = page.get(items_key) {
                for (scope, bucket) in scopes {
                    let bucket: T = serde_json::from_value(bucket.clone()).with_context(|| {
                        format!("Unexpected '{items_key}' in the response from {url}")
                    })?;
                    out.push((scope.clone(), bucket));
                }
            }
        }
        Ok(out)
    }

    // A list that never stops handing out tokens is a server fault, and
    // returning what was read so far would hide it behind a plausible list
    async fn pages(&self, url: &str) -> Result<Vec<serde_json::Value>> {
        let sep = if url.contains('?') { '&' } else { '?' };
        let mut pages = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page_url = match &token {
                Some(t) => format!("{url}{sep}pageToken={}", encode_query_value(t)),
                None => url.to_string(),
            };
            let page: serde_json::Value = self.get(&page_url).await?;
            let next = page
                .get("nextPageToken")
                .and_then(|t| t.as_str())
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            pages.push(page);
            match next {
                Some(t) => token = Some(t),
                None => return Ok(pages),
            }
        }
        Err(anyhow!(
            "The list at {url} did not end after {MAX_PAGES} pages; stopped so a partial list is not mistaken for the whole"
        ))
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

    // `None` is an answer with no operation in it (an empty body, or `{}`),
    // which means there is nothing to wait for. A body that is there but is
    // not an operation is an error: reading it as "nothing to wait for" is how
    // a teardown moved on while a delete was still running.
    pub async fn delete(&self, url: &str) -> Result<Option<Operation>> {
        let text = self.send(reqwest::Method::DELETE, url, None).await?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        let op: Operation = parse_json(&text, url)?;
        let names_nothing = op.name.is_none() && op.self_link.is_none();
        if names_nothing && op.error.is_none() && !op.is_done() {
            return Ok(None);
        }
        Ok(Some(op))
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

    fn status(code: u16, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(code).set_body_json(serde_json::json!({
            "error": {"code": code, "message": message}
        }))
    }

    fn thing() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({"name": "gt-dev-relay"}))
    }

    async fn requests_to(server: &MockServer) -> usize {
        server.received_requests().await.expect("requests").len()
    }

    #[tokio::test]
    async fn a_get_that_meets_a_503_then_a_200_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(status(503, "backend unavailable"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(thing())
            .mount(&server)
            .await;

        let got: Thing = client(&server)
            .get(&format!("{}/thing", server.uri()))
            .await
            .expect("the second attempt should win");
        assert_eq!(got.name, "gt-dev-relay");
        assert_eq!(requests_to(&server).await, 2);
    }

    #[tokio::test]
    async fn a_get_that_meets_a_503_four_times_fails_with_the_gcp_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(status(503, "backend unavailable").insert_header("retry-after", "0"))
            .mount(&server)
            .await;

        let err = client(&server)
            .get::<Thing>(&format!("{}/thing", server.uri()))
            .await
            .expect_err("four failures are a failure");
        let text = format!("{err:#}");
        assert!(text.contains("backend unavailable"), "got: {text}");
        assert!(text.contains("503"), "got: {text}");
        assert_eq!(requests_to(&server).await, 4, "four attempts, no more");
    }

    #[tokio::test]
    async fn a_429_and_a_500_are_retried_and_a_403_is_not() {
        for (code, retried) in [
            (429u16, true),
            (500, true),
            (502, true),
            (504, true),
            (403, false),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(status(code, "no"))
                .up_to_n_times(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .respond_with(thing())
                .mount(&server)
                .await;

            let out = client(&server)
                .get::<Thing>(&format!("{}/thing", server.uri()))
                .await;
            assert_eq!(out.is_ok(), retried, "status {code}: {out:?}");
        }
    }

    #[tokio::test]
    async fn a_post_that_meets_a_503_is_not_sent_twice() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(status(503, "backend unavailable"))
            .mount(&server)
            .await;

        let out = client(&server)
            .post::<_, Thing>(
                &format!("{}/thing", server.uri()),
                &serde_json::json!({"a": 1}),
            )
            .await;
        assert!(out.is_err());
        assert_eq!(
            requests_to(&server).await,
            1,
            "a create that GCP may have accepted must not be repeated"
        );
    }

    fn tokens(list: &[(&str, Option<u64>)]) -> TokenSource {
        TokenSource::Sequence(std::sync::Arc::new(std::sync::Mutex::new(
            list.iter()
                .map(|(token, secs)| IssuedToken {
                    token: (*token).to_string(),
                    valid_for: secs.map(Duration::from_secs),
                })
                .collect(),
        )))
    }

    #[tokio::test]
    async fn a_401_evicts_the_token_and_the_retry_carries_a_new_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer old"))
            .respond_with(status(401, "invalid credentials"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(header("authorization", "Bearer new"))
            .respond_with(thing())
            .mount(&server)
            .await;

        let client = GcpClient::new(
            tokens(&[("old", Some(3000)), ("new", Some(3000))]),
            Endpoints::single(&server.uri()),
        );
        let got: Thing = client
            .get(&format!("{}/thing", server.uri()))
            .await
            .expect("the fresh token should be accepted");
        assert_eq!(got.name, "gt-dev-relay");
        assert_eq!(requests_to(&server).await, 2);
    }

    #[tokio::test]
    async fn a_401_is_retried_once_not_forever() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(status(401, "invalid credentials"))
            .mount(&server)
            .await;

        let client = GcpClient::new(
            tokens(&[("a", Some(3000)), ("b", Some(3000)), ("c", Some(3000))]),
            Endpoints::single(&server.uri()),
        );
        let err = client
            .get::<Thing>(&format!("{}/thing", server.uri()))
            .await
            .expect_err("a second 401 is final");
        assert!(format!("{err:#}").contains("401"), "got: {err:#}");
        assert_eq!(requests_to(&server).await, 2);
    }

    async fn two_gets_carry(source: TokenSource) -> Vec<String> {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(thing())
            .mount(&server)
            .await;
        let client = GcpClient::new(source, Endpoints::single(&server.uri()));
        let url = format!("{}/thing", server.uri());
        client.get::<Thing>(&url).await.expect("first");
        client.get::<Thing>(&url).await.expect("second");
        server
            .received_requests()
            .await
            .expect("requests")
            .iter()
            .map(|r| {
                r.headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[tokio::test]
    async fn a_token_two_minutes_from_expiry_is_not_reused() {
        let got = two_gets_carry(tokens(&[("soon", Some(120)), ("later", Some(3000))])).await;
        assert_eq!(got, vec!["Bearer soon", "Bearer later"]);
    }

    #[tokio::test]
    async fn a_token_with_time_left_is_reused() {
        let got = two_gets_carry(tokens(&[("fine", Some(1800)), ("unused", Some(3000))])).await;
        assert_eq!(got, vec!["Bearer fine", "Bearer fine"]);
    }

    // The two cases below need a connection that breaks, which a mock server
    // cannot do, so a bare listener plays the part: each entry is what one
    // accepted connection is sent before it is closed, and "" is a reset
    async fn scripted(replies: Vec<&'static str>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) = listener.accept().await.expect("accept");
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                if !reply.is_empty() {
                    let _ = socket.write_all(reply.as_bytes()).await;
                }
            }
        });
        base
    }

    const DONE_OPERATION: &str = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
        content-length: 33\r\nconnection: close\r\n\r\n{\"name\":\"op-1\",\"status\":\"DONE\"}  ";

    #[tokio::test]
    async fn a_poll_that_sees_one_connection_reset_still_completes() {
        let base = scripted(vec!["", DONE_OPERATION]).await;
        let client = GcpClient::new(
            TokenSource::Static("tok".to_string()),
            Endpoints::single(&base),
        );
        let op = Operation {
            name: Some("op-1".to_string()),
            status: Some("PENDING".to_string()),
            self_link: Some(format!("{base}/operations/op-1")),
            error: None,
            done: None,
        };
        client
            .wait_operation(&op)
            .await
            .expect("one reset must not abandon a running operation");
    }

    // 40 bytes promised, 10 sent: the body read fails after a 200
    const CUT_BODY: &str = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
        content-length: 40\r\nconnection: close\r\n\r\n{\"name\":\"o";

    #[tokio::test]
    async fn a_delete_whose_body_is_cut_is_not_read_as_deleted() {
        let base = scripted(vec![CUT_BODY, DONE_OPERATION]).await;
        let client = GcpClient::new(
            TokenSource::Static("tok".to_string()),
            Endpoints::single(&base),
        );
        let op = client
            .delete(&format!("{base}/thing"))
            .await
            .expect("the retry reads the whole body");
        assert_eq!(
            op.and_then(|o| o.name).as_deref(),
            Some("op-1"),
            "the cut answer must not have become Ok(None)"
        );
    }

    #[tokio::test]
    async fn a_delete_answer_that_is_not_an_operation_is_an_error_and_an_empty_one_is_not() {
        for (body, expect) in [("", "none"), ("{}", "none"), ("<html>", "error")] {
            let server = MockServer::start().await;
            Mock::given(method("DELETE"))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server)
                .await;
            let out = client(&server)
                .delete(&format!("{}/thing", server.uri()))
                .await;
            match expect {
                "none" => assert!(matches!(out, Ok(None)), "{body:?}: {out:?}"),
                _ => assert!(out.is_err(), "{body:?}: {out:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_post_whose_body_is_cut_is_an_error_and_not_an_empty_success() {
        let base = scripted(vec![CUT_BODY]).await;
        let client = GcpClient::new(
            TokenSource::Static("tok".to_string()),
            Endpoints::single(&base),
        );
        let out = client
            .post::<_, ()>(&format!("{base}/thing"), &serde_json::json!({}))
            .await;
        // A unit type accepts an empty body, which is what a swallowed read
        // error used to look like
        assert!(out.is_err(), "got: {out:?}");
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

    // Pages are told apart by token, so a mock keyed on `pageToken` proves the
    // helper sent the token the server handed out and not its own count
    #[tokio::test]
    async fn get_all_follows_the_page_token_to_the_last_page() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/things"))
            .and(query_param("pageToken", "a+b/c="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"name": "two"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/things"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"name": "one"}],
                "nextPageToken": "a+b/c="
            })))
            .mount(&server)
            .await;

        let got: Vec<Thing> = client(&server)
            .get_all(&format!("{}/things", server.uri()), "items")
            .await
            .expect("get_all");
        let names: Vec<&str> = got.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["one", "two"]);
    }

    #[tokio::test]
    async fn get_all_keeps_a_filter_already_on_the_url() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/things"))
            .and(query_param("filter", "x"))
            .and(query_param("pageToken", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/things"))
            .and(query_param("filter", "x"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "nextPageToken": "p2"
            })))
            .mount(&server)
            .await;

        let got: Vec<Thing> = client(&server)
            .get_all(&format!("{}/things?filter=x", server.uri()), "items")
            .await
            .expect("get_all");
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn get_all_stops_at_the_page_cap_with_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/things"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"name": "again"}],
                "nextPageToken": "more"
            })))
            .mount(&server)
            .await;

        let err = client(&server)
            .get_all::<Thing>(&format!("{}/things", server.uri()), "items")
            .await
            .expect_err("a list that never ends is an error");
        assert!(format!("{err:#}").contains("50 pages"), "{err:#}");
        let seen = server.received_requests().await.expect("recorded").len();
        assert_eq!(seen, MAX_PAGES);
    }

    #[tokio::test]
    async fn get_all_scoped_merges_the_buckets_of_every_page() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/agg"))
            .and(query_param("pageToken", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": {"zones/b": {"name": "bee"}}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/agg"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": {"zones/a": {"name": "ay"}},
                "nextPageToken": "p2"
            })))
            .mount(&server)
            .await;

        let got: Vec<(String, Thing)> = client(&server)
            .get_all_scoped(&format!("{}/agg", server.uri()), "items")
            .await
            .expect("scoped");
        let scopes: Vec<&str> = got.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(scopes, vec!["zones/a", "zones/b"]);
    }
}
