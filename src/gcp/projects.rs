// Dead until `init` calls it, in the next commit
#![allow(dead_code)]

use anyhow::Result;
use serde::Deserialize;

use super::GcpClient;

// A large org can hold thousands of projects. Ten pages of 500 is far past
// what a person scrolls, and the picker filters as you type, so the cap bounds
// the wait without hiding a project the user can still type
const MAX_PAGES: usize = 10;
const PAGE_SIZE: u32 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub project_id: String,
    // Display name; empty when the project has none
    pub name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchPage {
    #[serde(default)]
    projects: Vec<RawProject>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProject {
    project_id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

// A page token is opaque and may carry `+`, `/` or `=`, so it cannot be
// pasted into a query string as it is
fn encode_query_value(raw: &str) -> String {
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

pub async fn list_projects(client: &GcpClient) -> Result<Vec<Project>> {
    let base = format!(
        "{}/projects:search?query=state:ACTIVE&pageSize={PAGE_SIZE}",
        client.endpoints.resource_manager
    );
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let url = match &token {
            Some(t) => format!("{base}&pageToken={}", encode_query_value(t)),
            None => base.clone(),
        };
        let page: SearchPage = client.get(&url).await?;
        for p in page.projects {
            // The query already asks for ACTIVE; check again so a project
            // pending deletion is never offered if the filter is ignored
            if p.state.as_deref().is_some_and(|s| s != "ACTIVE") {
                continue;
            }
            out.push(Project {
                project_id: p.project_id,
                name: p.display_name.unwrap_or_default(),
            });
        }
        match page.next_page_token.filter(|t| !t.is_empty()) {
            Some(next) => token = Some(next),
            None => break,
        }
    }
    out.sort_by(|a, b| a.project_id.cmp(&b.project_id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> GcpClient {
        GcpClient::new(
            crate::gcp::TokenSource::Static("tok".to_string()),
            crate::gcp::Endpoints::single(&server.uri()),
        )
    }

    #[tokio::test]
    async fn list_projects_returns_active_projects_sorted_by_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects:search"))
            .and(query_param("pageToken", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [
                    {"projectId": "example-dev-1a2b", "displayName": "Dev", "state": "ACTIVE"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/projects:search"))
            .and(query_param("query", "state:ACTIVE"))
            .and(query_param("pageSize", "500"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [
                    {"projectId": "example-prod-3c4d", "displayName": "Prod", "state": "ACTIVE"},
                    {"projectId": "example-bare-5e6f", "state": "ACTIVE"}
                ],
                "nextPageToken": "page-2"
            })))
            .mount(&server)
            .await;

        let got = list_projects(&client(&server)).await.expect("list");
        let ids: Vec<&str> = got.iter().map(|p| p.project_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["example-bare-5e6f", "example-dev-1a2b", "example-prod-3c4d"]
        );
        assert_eq!(got[0].name, "");
        assert_eq!(got[1].name, "Dev");
    }

    #[tokio::test]
    async fn list_projects_stops_at_the_page_cap() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects:search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [
                    {"projectId": "example-dev-1a2b", "displayName": "Dev", "state": "ACTIVE"}
                ],
                "nextPageToken": "more"
            })))
            .mount(&server)
            .await;

        let got = list_projects(&client(&server)).await.expect("no error");
        assert!(!got.is_empty());
        let seen = server.received_requests().await.expect("recorded").len();
        assert_eq!(seen, MAX_PAGES);
    }

    #[tokio::test]
    async fn list_projects_skips_a_project_that_is_not_active() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects:search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [
                    {"projectId": "example-dev-1a2b", "state": "ACTIVE"},
                    {"projectId": "example-old-9z9z", "state": "DELETE_REQUESTED"}
                ]
            })))
            .mount(&server)
            .await;

        let got = list_projects(&client(&server)).await.expect("list");
        let ids: Vec<&str> = got.iter().map(|p| p.project_id.as_str()).collect();
        assert_eq!(ids, vec!["example-dev-1a2b"]);
    }

    #[tokio::test]
    async fn list_projects_surfaces_a_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects:search"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": {"message": "Permission denied"}
            })))
            .mount(&server)
            .await;

        let err = list_projects(&client(&server)).await.expect_err("403");
        assert!(format!("{err:#}").contains("403"), "{err:#}");
    }
}
