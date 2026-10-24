use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use super::{is_not_found, GcpClient, Operation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsAuthorization {
    pub name: String,
    pub cname_name: String,
    pub cname_data: String,
}

#[derive(Deserialize)]
struct RawDnsAuthorization {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "dnsResourceRecord")]
    record: Option<RawDnsRecord>,
}

#[derive(Deserialize)]
struct RawDnsRecord {
    #[serde(default)]
    name: String,
    #[serde(default)]
    data: String,
}

impl From<RawDnsAuthorization> for DnsAuthorization {
    fn from(r: RawDnsAuthorization) -> Self {
        let record = r.record;
        DnsAuthorization {
            name: r.name,
            cname_name: record.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
            cname_data: record.map(|c| c.data).unwrap_or_default(),
        }
    }
}

fn parent(client: &GcpClient, project: &str) -> String {
    format!(
        "{}/projects/{project}/locations/global",
        client.endpoints.certmanager
    )
}

// The DNS authorization proves domain ownership; its CNAME goes into Cloud DNS
pub async fn ensure_dns_authorization(
    client: &GcpClient,
    project: &str,
    name: &str,
    domain: &str,
) -> Result<DnsAuthorization> {
    let base = format!("{}/dnsAuthorizations", parent(client, project));
    let url = format!("{base}/{name}");
    match client.get::<RawDnsAuthorization>(&url).await {
        Ok(raw) => return Ok(raw.into()),
        Err(e) if is_not_found(&e) => {}
        Err(e) => return Err(e),
    }
    let body = json!({"domain": domain, "type": "FIXED_RECORD"});
    let op: Operation = client
        .post(&format!("{base}?dnsAuthorizationId={name}"), &body)
        .await?;
    client
        .wait_operation_at(&op, &client.endpoints.certmanager)
        .await?;
    let raw: RawDnsAuthorization = client.get(&url).await?;
    Ok(raw.into())
}

pub async fn delete_dns_authorization(client: &GcpClient, project: &str, name: &str) -> Result<()> {
    let url = format!("{}/dnsAuthorizations/{name}", parent(client, project));
    match client.delete(&url).await {
        Ok(_) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

pub async fn ensure_certificate(
    client: &GcpClient,
    project: &str,
    name: &str,
    domains: &[String],
    dns_authorizations: &[String],
) -> Result<()> {
    let base = format!("{}/certificates", parent(client, project));
    let url = format!("{base}/{name}");
    match client.get::<serde_json::Value>(&url).await {
        Ok(_) => return Ok(()),
        Err(e) if is_not_found(&e) => {}
        Err(e) => return Err(e),
    }
    let body = json!({
        "managed": {"domains": domains, "dnsAuthorizations": dns_authorizations}
    });
    let op: Operation = client
        .post(&format!("{base}?certificateId={name}"), &body)
        .await?;
    client
        .wait_operation_at(&op, &client.endpoints.certmanager)
        .await
}

// A certificate that does not exist yet reads as MISSING, so doctor and the
// relay progress display can treat both cases the same way
pub async fn certificate_state(client: &GcpClient, project: &str, name: &str) -> Result<String> {
    let url = format!("{}/certificates/{name}", parent(client, project));
    match client.get::<serde_json::Value>(&url).await {
        Ok(v) => Ok(v
            .get("managed")
            .and_then(|m| m.get("state"))
            .and_then(|s| s.as_str())
            .unwrap_or("UNKNOWN")
            .to_string()),
        Err(e) if is_not_found(&e) => Ok("MISSING".to_string()),
        Err(e) => Err(e),
    }
}

pub async fn delete_certificate(client: &GcpClient, project: &str, name: &str) -> Result<()> {
    let url = format!("{}/certificates/{name}", parent(client, project));
    match client.delete(&url).await {
        Ok(_) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

pub async fn ensure_certificate_map(client: &GcpClient, project: &str, name: &str) -> Result<()> {
    let base = format!("{}/certificateMaps", parent(client, project));
    let url = format!("{base}/{name}");
    match client.get::<serde_json::Value>(&url).await {
        Ok(_) => return Ok(()),
        Err(e) if is_not_found(&e) => {}
        Err(e) => return Err(e),
    }
    let op: Operation = client
        .post(&format!("{base}?certificateMapId={name}"), &json!({}))
        .await?;
    client
        .wait_operation_at(&op, &client.endpoints.certmanager)
        .await
}

pub async fn delete_certificate_map(client: &GcpClient, project: &str, name: &str) -> Result<()> {
    let url = format!("{}/certificateMaps/{name}", parent(client, project));
    match client.delete(&url).await {
        Ok(_) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

pub async fn ensure_certificate_map_entry(
    client: &GcpClient,
    project: &str,
    map: &str,
    entry: &str,
    hostname: &str,
    certificate: &str,
) -> Result<()> {
    let base = format!(
        "{}/certificateMaps/{map}/certificateMapEntries",
        parent(client, project)
    );
    let url = format!("{base}/{entry}");
    match client.get::<serde_json::Value>(&url).await {
        Ok(_) => return Ok(()),
        Err(e) if is_not_found(&e) => {}
        Err(e) => return Err(e),
    }
    let body = json!({"hostname": hostname, "certificates": [certificate]});
    let op: Operation = client
        .post(&format!("{base}?certificateMapEntryId={entry}"), &body)
        .await?;
    client
        .wait_operation_at(&op, &client.endpoints.certmanager)
        .await
}

pub async fn delete_certificate_map_entry(
    client: &GcpClient,
    project: &str,
    map: &str,
    entry: &str,
) -> Result<()> {
    let url = format!(
        "{}/certificateMaps/{map}/certificateMapEntries/{entry}",
        parent(client, project)
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
    async fn ensure_dns_authorization_returns_the_existing_cname() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/projects/p/locations/global/dnsAuthorizations/gt-dev",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "projects/p/locations/global/dnsAuthorizations/gt-dev",
                "domain": "dev.example.com",
                "dnsResourceRecord": {
                    "name": "_acme-challenge.dev.example.com.",
                    "type": "CNAME",
                    "data": "1234.authorize.certificatemanager.goog."
                }
            })))
            .mount(&server)
            .await;

        let auth = ensure_dns_authorization(&client(&server), "p", "gt-dev", "dev.example.com")
            .await
            .expect("auth");
        assert_eq!(auth.cname_name, "_acme-challenge.dev.example.com.");
        assert_eq!(auth.cname_data, "1234.authorize.certificatemanager.goog.");
    }

    #[tokio::test]
    async fn certificate_state_reports_the_provisioning_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/locations/global/certificates/gt-dev"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "projects/p/locations/global/certificates/gt-dev",
                "managed": {"state": "PROVISIONING"}
            })))
            .mount(&server)
            .await;

        let state = certificate_state(&client(&server), "p", "gt-dev")
            .await
            .expect("state");
        assert_eq!(state, "PROVISIONING");
    }

    #[tokio::test]
    async fn certificate_state_is_missing_when_the_certificate_does_not_exist() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": 404, "message": "not found"}
            })))
            .mount(&server)
            .await;

        let state = certificate_state(&client(&server), "p", "gt-dev")
            .await
            .expect("state");
        assert_eq!(state, "MISSING");
    }
}
