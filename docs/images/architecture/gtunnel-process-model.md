---
title: "Process model"
subtitle: "One supervised service per tunnel, one watchdog for the machine"
date: "2026-10-27"
team: "gtunnel"
audience: "Engineering"
classification: "Public"
tags: [architecture, gcp]
---

## Process model

:::diagram-architecture "Process model"
nodes:
- id: local, label: "Your laptop", style: external, type: container
- id: cli, label: "gtunnel", subtitle: "CLI / TUI", icon: laptop, parent: local
- id: state, label: "tunnels.toml", subtitle: "and config.toml", icon: storage, parent: local
- id: svc, label: "Per-tunnel service", subtitle: "launchd plist / systemd unit", icon: service, parent: local
- id: sup, label: "gtunnel __supervise", subtitle: "supervisor", icon: job, parent: local
- id: proc, label: "gcloud", subtitle: "child process", icon: server, parent: local
- id: log, label: "Tunnel log", subtitle: "logs/<account>-<name>.log", icon: logging, parent: local
- id: wd, label: "Machine watchdog", subtitle: "StartInterval / systemd timer", icon: scheduler, parent: local
- id: once, label: "gtunnel watch --once", subtitle: "one pass, then exits", icon: cronjob, parent: local
- id: brk, label: "breakers.toml", subtitle: "restart ladder per tunnel", icon: storage, parent: local
- id: gcp, label: "GCP example-prod-3c4d", style: gcp, type: container
- id: api_compute, label: "Compute", subtitle: "REST API", icon: compute, parent: gcp
- id: api_dns, label: "Cloud DNS", subtitle: "REST API", icon: dns, parent: gcp
- id: api_certs, label: "Certificate Manager", subtitle: "REST API", icon: certmanager, parent: gcp
- id: api_iap, label: "IAP", subtitle: "REST API", icon: iap, parent: gcp
- id: api_mon, label: "Monitoring", subtitle: "REST API", icon: monitoring, parent: gcp
edges:
- from: cli, to: state
- from: cli, to: svc
- from: svc, to: sup
- from: sup, to: proc
- from: sup, to: log
- from: cli, to: log, label: "reads", style: dashed
- from: cli, to: gcp, label: "polls", style: dashed
- from: wd, to: once
- from: once, to: svc, label: "probes", style: dashed
- from: once, to: brk
- from: once, to: log
:::
