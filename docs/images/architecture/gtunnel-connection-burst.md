---
title: "Connection burst"
subtitle: "What each tunnel shape does with many local connections"
date: "2026-10-29"
team: "gtunnel"
audience: "Engineering"
classification: "Public"
tags: [architecture, gcp]
---

## Connection burst

:::diagram-architecture "Connection burst"
nodes:
- id: local, label: "Your laptop", style: external, type: container
- id: clients, label: "Local clients", subtitle: "many, to localhost", icon: users, parent: local
- id: direct, label: "gcloud, direct", subtitle: "no cap, backlog 128", icon: server, parent: local
- id: jump, label: "ssh, jump", subtitle: "N channels, one stream", icon: server, parent: local
- id: sup, label: "gtunnel supervisor", subtitle: "restarts the process", icon: job, parent: local
- id: wd, label: "Machine watchdog", subtitle: "one pass every 30s", icon: scheduler, parent: local
- id: fds, label: "Descriptor limit", subtitle: "4096 per service", icon: service, parent: local
- id: gcp, label: "GCP example-dev-1a2b", style: gcp, type: container
- id: iap, label: "IAP", subtitle: "no published quota", icon: iap, parent: gcp
- id: vpc, label: "VPC (europe-west1)", style: vpc, type: container, parent: gcp
- id: vm, label: "my-instance", subtitle: "direct target", icon: compute, parent: vpc
- id: bastion, label: "bastion", subtitle: "jump target", icon: compute, parent: vpc
edges:
- from: clients, to: direct, label: "N connections"
- from: clients, to: jump, label: "N connections"
- from: direct, to: iap, label: "WebSocket each"
- from: jump, to: iap, label: "one WebSocket"
- from: iap, to: vm, label: "N streams"
- from: iap, to: bastion, label: "one stream"
- from: sup, to: direct, label: "supervises", style: dashed
- from: sup, to: jump, label: "supervises", style: dashed
- from: wd, to: sup, label: "probes", style: dashed
- from: fds, to: sup, label: "bounds", style: dashed
:::
