---
title: "MCP server"
subtitle: "An agent's calls, from stdin to a tunnel"
date: "2026-10-27"
team: "gtunnel"
audience: "Engineering"
classification: "Public"
tags: [architecture, mcp]
---

## MCP server

:::diagram-architecture "MCP server"
nodes:
- id: local, label: "Your laptop", style: external, type: container
- id: client, label: "MCP client", subtitle: "an agent", icon: llm, parent: local
- id: server, label: "gtunnel mcp", subtitle: "JSON-RPC 2.0 over stdio", icon: mcp, parent: local
- id: table, label: "Tool table", subtitle: "src/mcp/tools.rs", icon: service, parent: local
- id: state, label: "tunnels.toml", subtitle: "and config.toml", icon: storage, parent: local
- id: svc, label: "Per-tunnel service", subtitle: "launchd / systemd", icon: service, parent: local
- id: run, label: "gtunnel run", subtitle: "own process group, TTL", icon: job, parent: local
- id: rec, label: "Run record", subtitle: "run/<acct>-<name>.json", icon: storage, parent: local
- id: log, label: "Run log", subtitle: "logs/run-<acct>-<name>.log", icon: logging, parent: local
- id: gcp, label: "GCP example-dev-1a2b", style: gcp, type: container
- id: iap, label: "IAP", subtitle: "TCP forwarding", icon: iap, parent: gcp
edges:
- from: client, to: server, label: "stdin / stdout"
- from: server, to: table
- from: table, to: state, label: "locked writes"
- from: table, to: svc, label: "start, stop"
- from: table, to: run, label: "run_tunnel"
- from: run, to: rec
- from: run, to: log
- from: server, to: rec, label: "waits up to 5s", style: dashed
- from: run, to: iap, style: dashed
:::
