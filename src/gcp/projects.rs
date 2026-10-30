use anyhow::Result;
use serde::Deserialize;

use super::GcpClient;

const PAGE_SIZE: u32 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub project_id: String,
    // Display name; empty when the project has none
    pub name: String,
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

pub async fn list_projects(client: &GcpClient) -> Result<Vec<Project>> {
    let base = format!(
        "{}/projects:search?query=state:ACTIVE&pageSize={PAGE_SIZE}",
        client.endpoints.resource_manager
    );
    let raw: Vec<RawProject> = client.get_all(&base, "projects").await?;
    let mut out = Vec::new();
    for p in raw {
        // The query already asks for ACTIVE; check again so a project pending
        // deletion is never offered if the filter is ignored
        if p.state.as_deref().is_some_and(|s| s != "ACTIVE") {
            continue;
        }
        out.push(Project {
            project_id: p.project_id,
            name: p.display_name.unwrap_or_default(),
        });
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
    async fn list_projects_stops_at_the_shared_page_cap_with_an_error() {
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

        let err = list_projects(&client(&server)).await.expect_err("cap");
        assert!(format!("{err:#}").contains("50 pages"), "{err:#}");
        let seen = server.received_requests().await.expect("recorded").len();
        assert_eq!(seen, 50);
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
