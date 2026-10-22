use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{is_not_found, GcpClient};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ManagedZone {
    pub name: String,
    #[serde(rename = "dnsName")]
    pub dns_name: String,
}

#[derive(Deserialize)]
struct ManagedZoneList {
    #[serde(default, rename = "managedZones")]
    managed_zones: Vec<ManagedZone>,
}

pub async fn list_managed_zones(client: &GcpClient, project: &str) -> Result<Vec<ManagedZone>> {
    let url = format!("{}/projects/{project}/managedZones", client.endpoints.dns);
    let raw: ManagedZoneList = client.get(&url).await?;
    Ok(raw.managed_zones)
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

#[derive(Deserialize)]
struct RecordSetList {
    #[serde(default)]
    rrsets: Vec<RecordSet>,
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
    let raw: RecordSetList = client.get(&url).await?;
    Ok(raw.rrsets)
}

pub async fn create_record_set(
    client: &GcpClient,
    project: &str,
    zone: &str,
    record: &RecordSet,
) -> Result<RecordSet> {
    let url = format!(
        "{}/projects/{project}/managedZones/{zone}/rrsets",
        client.endpoints.dns
    );
    client.post(&url, record).await
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
}
