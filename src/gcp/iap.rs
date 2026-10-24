use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use super::GcpClient;

pub const INSTANCE_PERMISSION: &str = "iap.tunnelInstances.accessViaIAP";
pub const DEST_GROUP_PERMISSION: &str = "iap.tunnelDestGroups.accessViaIAP";

#[derive(Deserialize)]
struct TestIamPermissionsResponse {
    #[serde(default)]
    permissions: Vec<String>,
}

// testIamPermissions echoes back only the permissions the caller holds, so an
// empty response means the IAM binding is missing: the cause of a 4033
async fn has_permission(client: &GcpClient, url: &str, permission: &str) -> Result<bool> {
    let body = json!({"permissions": [permission]});
    let resp: TestIamPermissionsResponse = client.post(url, &body).await?;
    Ok(resp.permissions.iter().any(|p| p == permission))
}

pub async fn can_tunnel_instance(
    client: &GcpClient,
    project: &str,
    zone: &str,
    instance: &str,
) -> Result<bool> {
    let url = format!(
        "{}/projects/{project}/iap_tunnel/zones/{zone}/instances/{instance}:testIamPermissions",
        client.endpoints.iap
    );
    has_permission(client, &url, INSTANCE_PERMISSION).await
}

pub async fn can_tunnel_dest_group(
    client: &GcpClient,
    project: &str,
    region: &str,
    group: &str,
) -> Result<bool> {
    let url = format!(
        "{}/projects/{project}/iap_tunnel/locations/{region}/destGroups/{group}:testIamPermissions",
        client.endpoints.iap
    );
    has_permission(client, &url, DEST_GROUP_PERMISSION).await
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
    async fn can_tunnel_instance_is_true_when_the_permission_is_echoed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/projects/p/iap_tunnel/zones/europe-west1-b/instances/db:testIamPermissions",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "permissions": ["iap.tunnelInstances.accessViaIAP"]
            })))
            .mount(&server)
            .await;

        let ok = can_tunnel_instance(&client(&server), "p", "europe-west1-b", "db")
            .await
            .expect("call");
        assert!(ok);
    }

    #[tokio::test]
    async fn can_tunnel_instance_is_false_when_the_permission_is_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let ok = can_tunnel_instance(&client(&server), "p", "europe-west1-b", "db")
            .await
            .expect("call");
        assert!(!ok);
    }

    #[tokio::test]
    async fn can_tunnel_dest_group_uses_the_dest_group_permission() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/projects/p/iap_tunnel/locations/europe-west1/destGroups/db:testIamPermissions",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "permissions": ["iap.tunnelDestGroups.accessViaIAP"]
            })))
            .mount(&server)
            .await;

        let ok = can_tunnel_dest_group(&client(&server), "p", "europe-west1", "db")
            .await
            .expect("call");
        assert!(ok);
    }
}
