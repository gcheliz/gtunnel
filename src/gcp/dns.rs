use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::client::encode_query_value;
use super::{is_not_found, GcpClient};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ManagedZone {
    pub name: String,
    #[serde(rename = "dnsName")]
    pub dns_name: String,
}

pub async fn list_managed_zones(client: &GcpClient, project: &str) -> Result<Vec<ManagedZone>> {
    let url = format!("{}/projects/{project}/managedZones", client.endpoints.dns);
    client.get_all(&url, "managedZones").await
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordSet {
    pub name: String,
    #[serde(rename = "type")]
    pub rtype: String,
    #[serde(default)]
    pub ttl: u32,
    #[serde(default)]
    pub rrdatas: Vec<String>,
}

pub async fn list_record_sets(
    client: &GcpClient,
    project: &str,
    zone: &str,
) -> Result<Vec<RecordSet>> {
    let url = format!(
        "{}/projects/{project}/managedZones/{zone}/rrsets",
        client.endpoints.dns
    );
    client.get_all(&url, "rrsets").await
}

/// One record by name and type. The route code asks this instead of listing
/// the zone, so a zone larger than a page can neither hide the record nor cost
/// a walk of every page to find it. The answer is filtered again here because
/// the name is what an existing-record decision rests on.
pub async fn find_record_set(
    client: &GcpClient,
    project: &str,
    zone: &str,
    name: &str,
    rtype: &str,
) -> Result<Option<RecordSet>> {
    let url = format!(
        "{}/projects/{project}/managedZones/{zone}/rrsets?name={}&type={}",
        client.endpoints.dns,
        encode_query_value(name),
        encode_query_value(rtype)
    );
    let found: Vec<RecordSet> = client.get_all(&url, "rrsets").await?;
    Ok(found
        .into_iter()
        .find(|r| r.name == name && r.rtype == rtype))
}

pub async fn create_record_set(
    client: &GcpClient,
    project: &str,
    zone: &str,
    record: &RecordSet,
) -> Result<()> {
    let url = format!(
        "{}/projects/{project}/managedZones/{zone}/rrsets",
        client.endpoints.dns
    );
    // The created record is echoed back, and no caller needs it
    let _: serde_json::Value = client.post(&url, record).await?;
    Ok(())
}

// Deleting a record that is already gone is the desired end state, not an error
pub async fn delete_record_set(
    client: &GcpClient,
    project: &str,
    zone: &str,
    name: &str,
    rtype: &str,
) -> Result<()> {
    let url = format!(
        "{}/projects/{project}/managedZones/{zone}/rrsets/{name}/{rtype}",
        client.endpoints.dns
    );
    match client.delete(&url).await {
        Ok(_) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcp::{Endpoints, GcpClient, TokenSource};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> GcpClient {
        GcpClient::new(
            TokenSource::Static("tok".to_string()),
            Endpoints::single(&server.uri()),
        )
    }

    #[tokio::test]
    async fn list_managed_zones_parses_name_and_dns_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/managedZones"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "managedZones": [
                    {"name": "example-dev", "dnsName": "dev.example.com.", "visibility": "public"}
                ]
            })))
            .mount(&server)
            .await;

        let zones = list_managed_zones(&client(&server), "p")
            .await
            .expect("zones");
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].name, "example-dev");
        assert_eq!(zones[0].dns_name, "dev.example.com.");
    }

    #[tokio::test]
    async fn create_record_set_posts_the_record() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/projects/p/managedZones/example-dev/rrsets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "api.dev.example.com.",
                "type": "A",
                "ttl": 300,
                "rrdatas": ["34.1.2.3"]
            })))
            .mount(&server)
            .await;

        let record = RecordSet {
            name: "api.dev.example.com.".to_string(),
            rtype: "A".to_string(),
            ttl: 300,
            rrdatas: vec!["34.1.2.3".to_string()],
        };
        create_record_set(&client(&server), "p", "example-dev", &record)
            .await
            .expect("create");
    }

    #[tokio::test]
    async fn delete_record_set_is_quiet_when_the_record_is_gone() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": 404, "message": "not found"}
            })))
            .mount(&server)
            .await;

        delete_record_set(
            &client(&server),
            "p",
            "example-dev",
            "api.dev.example.com.",
            "A",
        )
        .await
        .expect("delete");
    }

    #[tokio::test]
    async fn a_record_on_the_second_page_of_a_zone_is_found() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/managedZones/example-dev/rrsets"))
            .and(query_param("pageToken", "page-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "rrsets": [{"name": "late.dev.example.com.", "type": "A", "ttl": 60,
                            "rrdatas": ["34.1.2.3"]}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/projects/p/managedZones/example-dev/rrsets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "rrsets": [{"name": "early.dev.example.com.", "type": "A", "ttl": 60,
                            "rrdatas": ["34.1.2.4"]}],
                "nextPageToken": "page-2"
            })))
            .mount(&server)
            .await;

        let got = list_record_sets(&client(&server), "p", "example-dev")
            .await
            .expect("list");
        assert!(got.iter().any(|r| r.name == "late.dev.example.com."));
    }

    #[tokio::test]
    async fn find_record_set_asks_for_one_name_and_type() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/managedZones/example-dev/rrsets"))
            .and(query_param("name", "api.dev.example.com."))
            .and(query_param("type", "A"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "rrsets": [{"name": "api.dev.example.com.", "type": "A", "ttl": 60,
                            "rrdatas": ["34.1.2.3"]}]
            })))
            .mount(&server)
            .await;

        let got = find_record_set(
            &client(&server),
            "p",
            "example-dev",
            "api.dev.example.com.",
            "A",
        )
        .await
        .expect("find");
        assert_eq!(got.map(|r| r.rrdatas), Some(vec!["34.1.2.3".to_string()]));
    }

    #[tokio::test]
    async fn find_record_set_is_none_when_the_zone_has_no_such_record() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let got = find_record_set(&client(&server), "p", "example-dev", "x.example.com.", "A")
            .await
            .expect("find");
        assert_eq!(got, None);
    }
}
