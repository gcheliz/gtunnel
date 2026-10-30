// Commands that call the GCP REST API, run as a person types them against a
// wiremock server standing in for Google. What these prove is what no
// in-process test can: the exit code, the output and the saved file together,
// with the real client built the way every command builds it.
//
// The account names here are used by no other test, and no tunnel is ever
// started: a launchd label is `com.gtunnel.<account>.<name>`, and a test must
// not touch one a real config might own.

mod common;

use common::{text, Harness, LOGGED_IN_STUB};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROJECT: &str = "example-dev-1a2b";

// An empty default zone is how a config says there is none (the key itself is
// required): the zone has to come from the lookup, or the command fails
const NO_DEFAULT_ZONE: &str = r#"selected_account = "fakegcp"

[[accounts]]
name = "fakegcp"
gcloud_configuration = "work"
project = "example-dev-1a2b"
default_zone = ""
"#;

fn harness(server: &MockServer, config: &str, tunnels: Option<&str>) -> Harness {
    Harness::build(&[("gcloud", LOGGED_IN_STUB)], config, tunnels).with_gcp(server)
}

fn instance_in(zone: &str) -> serde_json::Value {
    json!({
        "name": "bastion",
        "status": "RUNNING",
        "zone": format!("https://www.googleapis.com/compute/v1/projects/{PROJECT}/zones/{zone}"),
    })
}

async fn serve_instances(server: &MockServer, found: &[(&str, &str)]) {
    let mut items = serde_json::Map::new();
    for (zone, name) in found {
        let mut one = instance_in(zone);
        one["name"] = json!(name);
        items.insert(format!("zones/{zone}"), json!({ "instances": [one] }));
    }
    Mock::given(method("GET"))
        .and(path(format!("/projects/{PROJECT}/aggregated/instances")))
        .and(query_param("filter", "name=\"bastion\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": items })))
        .mount(server)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn add_saves_the_zone_the_compute_api_reports_for_the_instance() {
    let server = MockServer::start().await;
    serve_instances(&server, &[("europe-west1-b", "bastion")]).await;
    let h = harness(&server, NO_DEFAULT_ZONE, None);

    let out = h.run(&["add", "pg", "10.0.0.10:5432", "--via", "bastion"]);

    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    let saved = std::fs::read_to_string(h.config_path("tunnels.toml")).unwrap();
    assert!(saved.contains("instance = \"bastion\""), "{saved}");
    assert!(saved.contains("zone = \"europe-west1-b\""), "{saved}");
}

#[tokio::test(flavor = "multi_thread")]
async fn add_exits_2_when_the_instance_is_in_several_zones() {
    let server = MockServer::start().await;
    serve_instances(
        &server,
        &[("europe-west1-b", "bastion"), ("us-central1-a", "bastion")],
    )
    .await;
    let h = harness(&server, NO_DEFAULT_ZONE, None);

    let out = h.run(&["add", "pg", "10.0.0.10:5432", "--via", "bastion"]);

    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr: {err}");
    assert!(err.contains("europe-west1-b, us-central1-a"), "{err}");
    assert!(err.contains("--zone"), "{err}");
    assert!(
        !h.config_path("tunnels.toml").exists(),
        "a refused add must save nothing"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn add_exits_3_when_the_project_has_no_such_instance() {
    let server = MockServer::start().await;
    serve_instances(&server, &[]).await;
    let h = harness(&server, NO_DEFAULT_ZONE, None);

    let out = h.run(&["add", "pg", "10.0.0.10:5432", "--via", "bastion"]);

    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {err}");
    assert!(err.contains("Instance 'bastion' not found"), "{err}");
    assert!(!h.config_path("tunnels.toml").exists());
}

const DOCTOR_CONFIG: &str = r#"selected_account = "fakedoctor"

[[accounts]]
name = "fakedoctor"
gcloud_configuration = "work"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"
"#;

const DOCTOR_TUNNELS: &str = r#"[[tunnels]]
name = "pg"
account_name = "fakedoctor"
auto_start = false

[tunnels.spec]
mode = "access"
remote_port = 5432
local_port = 15432

[tunnels.spec.target]
kind = "instance"
instance = "bastion"
zone = "europe-west1-b"
"#;

#[tokio::test(flavor = "multi_thread")]
async fn doctor_reports_a_tunnel_whose_port_has_no_iap_firewall_rule() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/projects/{PROJECT}/zones/europe-west1-b/instances/bastion"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(instance_in("europe-west1-b")))
        .mount(&server)
        .await;
    // The only rule admits IAP on port 22, so a tunnel to 5432 has none
    Mock::given(method("GET"))
        .and(path(format!("/projects/{PROJECT}/global/firewalls")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [{
                "name": "allow-iap-ssh",
                "network": "default",
                "direction": "INGRESS",
                "sourceRanges": ["35.235.240.0/20"],
                "allowed": [{ "IPProtocol": "tcp", "ports": ["22"] }],
            }]
        })))
        .mount(&server)
        .await;
    // IAM is in order, so the firewall is the only finding about this tunnel
    Mock::given(method("POST"))
        .and(path(format!(
            "/projects/{PROJECT}/iap_tunnel/zones/europe-west1-b/instances/bastion:testIamPermissions"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "permissions": ["iap.tunnelInstances.accessViaIAP"]
        })))
        .mount(&server)
        .await;
    let h = harness(&server, DOCTOR_CONFIG, Some(DOCTOR_TUNNELS));

    let out = h.run(&["doctor"]);

    let all = format!("{}{}", text(&out.stdout), text(&out.stderr));
    assert!(
        all.contains("Tunnel 'pg' has no firewall rule for IAP"),
        "{all}"
    );
    assert!(
        all.contains("gcloud compute firewall-rules create gt-allow-iap-5432"),
        "{all}"
    );
    assert!(!all.contains("IAM"), "IAM was granted: {all}");
}
