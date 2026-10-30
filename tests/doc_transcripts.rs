// The command transcripts in docs/ run against the binary, so a changed
// message or flag fails here instead of leaving a page quietly wrong.
//
// Only a fence marked `console` is run; a plain fence stays prose, which is how
// a transcript that needs a live tunnel, launchd state, a prompt answered or
// real GCP is kept out. A file is one sequence: its blocks run in order against
// one scratch config, so a block may rely on what an earlier block of the same
// page wrote, and a page is ordered to allow it. Pages never share a scratch
// directory, because trycmd runs files in parallel.
//
// macOS only, as the pages are: the service manager they show is launchd, and a
// `launchctl` stub heads the PATH so no real job is ever read or written.
#![cfg(target_os = "macos")]

mod common;

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use common::Harness;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROJECT: &str = "example-dev-1a2b";

// gcloud as `init` reads it: one configuration, `dev`, active, with a project
// and no zone, so the zone step is the one a person meets on a fresh machine
const DOC_GCLOUD: &str = r#"#!/bin/sh
case "$1 $2" in
  "auth print-access-token")
    echo stub-token
    exit 0
    ;;
  "config configurations")
    cat <<'JSON'
[{"name":"sandbox","is_active":true,"properties":{"core":{"project":"example-dev-1a2b","account":"you@example.com"},"compute":{}}}]
JSON
    exit 0
    ;;
esac
exit 0
"#;

// A launchd with nothing loaded: `print` says so the way launchd does, and
// every other verb succeeds without touching the machine's own jobs
const EMPTY_LAUNCHD: &str = r#"#!/bin/sh
if [ "$1" = "print" ]; then
  echo "Could not find service" >&2
  exit 113
fi
exit 0
"#;

// `dev` and `prod` exist before the page starts, so a block can name either,
// and `init` adds a third beside them
const DEV_AND_PROD: &str = r#"selected_account = "dev"

[[accounts]]
name = "dev"
gcloud_configuration = "dev"
project = "example-dev-1a2b"
default_zone = "europe-west1-b"

[[accounts]]
name = "prod"
gcloud_configuration = "prod"
project = "example-prod-3c4d"
default_zone = "europe-west1-b"
"#;

const DB_TUNNEL: &str = r#"[[tunnels]]
name = "db"
account_name = "dev"
auto_start = false

[tunnels.spec]
mode = "access"
remote_port = 5432
local_port = 15432

[tunnels.spec.target]
kind = "instance"
instance = "postgres-1"
zone = "europe-west1-b"
"#;

/// A scratch home for one page: gtunnel's own directory is `~/.gtunnel`
/// there, so a path in the output reads as it does on a machine.
fn scratch(config: Option<&str>, tunnels: Option<&str>) -> Harness {
    let h = Harness::build(
        &[("gcloud", DOC_GCLOUD), ("launchctl", EMPTY_LAUNCHD)],
        config.unwrap_or(""),
        tunnels,
    );
    let root = h.root().to_path_buf();
    fs::rename(root.join("config"), root.join(".gtunnel")).unwrap();
    if config.is_none() {
        fs::remove_file(root.join(".gtunnel/config.toml")).unwrap();
    }
    h
}

fn cases(h: &Harness, server: &MockServer) -> trycmd::TestCases {
    let root = h.root();
    let system = "/usr/bin:/bin:/usr/sbin:/sbin";
    let cases = trycmd::TestCases::new();
    cases
        .register_bin("gtunnel", Path::new(env!("CARGO_BIN_EXE_gtunnel")))
        .env("PATH", format!("{}:{system}", root.join("bin").display()))
        .env("HOME", root.display().to_string())
        .env("GTUNNEL_TEST_GCP_ENDPOINT", server.uri())
        .env("STUB_DIR", root.join("stub").display().to_string());
    // The scratch root is the only part of a path that differs between runs, so
    // it is the only part a placeholder hides
    cases
        .insert_var("[HOME]", root.display().to_string())
        .unwrap();
    cases
}

async fn serve_the_dev_bastion(server: &MockServer) {
    // Prod answers nothing for the bastion, so wiremock's 404 is the "not found"
    // a block shows; dev has it, running, in the account's zone
    Mock::given(method("GET"))
        .and(path(format!(
            "/projects/{PROJECT}/zones/europe-west1-b/instances/bastion"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "bastion",
            "status": "RUNNING",
        })))
        .mount(server)
        .await;
}

// A local port a transcript binds. `add` refuses one that something on the
// machine already listens on, so a developer's own ssh tunnel on 8443 would fail
// a page that is right; the page keeps reading this port and the run swaps in a
// free one. Remote ports (`10.0.0.10:443`) are not listed: nothing binds them.
const DOCUMENTED_LOCAL_PORTS: [u16; 1] = [8443];

/// A port nothing listens on, on both loopback families: `add` probes `localhost`,
/// which resolves to either, so a port free on IPv4 alone is still refused.
fn free_local_port() -> u16 {
    loop {
        let v4 = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = v4.local_addr().unwrap().port();
        // A host without IPv6 cannot be holding the port there
        if TcpListener::bind(("::1", port)).is_ok() || !ipv6_loopback_exists() {
            return port;
        }
    }
}

fn ipv6_loopback_exists() -> bool {
    TcpListener::bind(("::1", 0)).is_ok()
}

/// The page with each documented local port replaced by a free one, written into
/// the scratch root so trycmd reads the substituted copy, and the original stays
/// byte for byte what a reader sees. Output is substituted too, being the same text.
fn page_with_free_ports(page: &str, h: &Harness) -> PathBuf {
    let mut text = fs::read_to_string(page).unwrap();
    for port in DOCUMENTED_LOCAL_PORTS {
        text = text.replace(&port.to_string(), &free_local_port().to_string());
    }
    let copy = h.root().join(Path::new(page).file_name().unwrap());
    fs::write(&copy, text).unwrap();
    copy
}

// The pages whose fences this file owns. A bare fence in one is an error
// (trycmd would read it as a transcript), so each block says what it is:
// `console` runs, `console,ignore` is a transcript that cannot run offline,
// anything else is not a transcript.
const PAGES: [&str; 3] = ["docs/commands.md", "docs/usage.md", "docs/configuration.md"];

// Held to the same rule for a `$ gtunnel` block, though none of its blocks runs:
// it has no fixture, so its fences stay as they were
const UNCHECKED_PAGES: [&str; 1] = ["docs/troubleshooting.md"];

async fn check_the_command_reference() {
    let server = MockServer::start().await;
    serve_the_dev_bastion(&server).await;
    let h = scratch(Some(DEV_AND_PROD), Some(DB_TUNNEL));
    let page = page_with_free_ports("docs/commands.md", &h);
    cases(&h, &server).case(page).run();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_reference_transcripts_match_the_binary() {
    check_the_command_reference().await;
}

// Nothing is configured, as on a machine that has just installed the binary
#[tokio::test(flavor = "multi_thread")]
async fn the_usage_transcripts_match_the_binary() {
    let server = MockServer::start().await;
    let h = scratch(None, None);
    cases(&h, &server).case("docs/usage.md").run();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_configuration_transcripts_match_the_binary() {
    let server = MockServer::start().await;
    let h = scratch(None, None);
    cases(&h, &server).case("docs/configuration.md").run();
}

/// The opening line of every fenced block of a page, with its line number.
fn fences(page: &str) -> Vec<(usize, String, Option<String>)> {
    let text = fs::read_to_string(page).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut open = false;
    for (i, line) in lines.iter().enumerate() {
        if let Some(info) = line.strip_prefix("```") {
            if !open {
                found.push((
                    i + 1,
                    info.trim().to_string(),
                    lines.get(i + 1).map(|l| l.to_string()),
                ));
            }
            open = !open;
        }
    }
    found
}

#[test]
fn no_page_leaves_a_fence_for_trycmd_to_guess_at() {
    for page in PAGES {
        for (line, info, _) in fences(page) {
            assert!(
                !info.is_empty(),
                "{page}:{line}: a bare fence is read as a transcript; use `console` to check it, `console,ignore` for a transcript that cannot run offline, `text` for anything else"
            );
        }
    }
}

#[test]
fn a_gtunnel_transcript_is_either_checked_or_marked_unchecked() {
    for page in PAGES.iter().chain(UNCHECKED_PAGES.iter()) {
        for (line, info, first) in fences(page) {
            let is_transcript = first.is_some_and(|l| l.starts_with("$ gtunnel"));
            if is_transcript {
                assert!(
                    info == "console" || info == "console,ignore",
                    "{page}:{line}: a `$ gtunnel` block fenced `{info}` is neither run nor marked as skipped"
                );
            }
        }
    }
}

#[test]
fn the_checked_blocks_are_not_all_quietly_switched_off() {
    let checked: usize = PAGES
        .iter()
        .map(|page| {
            fences(page)
                .iter()
                .filter(|(_, i, _)| i == "console")
                .count()
        })
        .sum();
    assert!(checked >= 20, "only {checked} checked blocks are left");
}

#[test]
fn a_free_port_is_never_one_that_something_listens_on() {
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let taken = held.local_addr().unwrap().port();
    for _ in 0..50 {
        let port = free_local_port();
        assert_ne!(port, taken);
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok());
    }
}

// Holds the documented port itself, as a developer's ssh tunnel does, and runs
// the page: it must still pass. If something else already holds it, that is the
// same situation, so a failed bind is not an error.
#[tokio::test(flavor = "multi_thread")]
async fn the_command_reference_passes_while_its_documented_port_is_held() {
    let _held = DOCUMENTED_LOCAL_PORTS.map(|p| TcpListener::bind(("127.0.0.1", p)).ok());
    check_the_command_reference().await;
}

#[test]
fn the_fixture_names_only_placeholder_projects() {
    assert!(DOC_GCLOUD.contains(PROJECT));
    assert!(DEV_AND_PROD.contains("example-prod-3c4d"));
}
