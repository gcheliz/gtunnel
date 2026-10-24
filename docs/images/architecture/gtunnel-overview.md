---
title: "gtunnel topology"
subtitle: "Both modes: a private port inward, a public URL outward"
date: "2026-10-24"
team: "Platform Engineering"
audience: "Engineering"
classification: "Internal"
tags: [architecture, gcp]
---

## gtunnel topology

:::diagram-architecture "gtunnel topology"
nodes:
- id: net, label: "Internet", style: external, type: container
- id: visitor, label: "Visitors", subtitle: "web.example.com", icon: users, parent: net
- id: local, label: "Your laptop", style: external, type: container
- id: cli, label: "gtunnel", subtitle: "TUI and user services", icon: laptop, parent: local
- id: client, label: "Your client", subtitle: "psql, kubectl, Compass", icon: server, parent: local
- id: svc, label: "Local service", subtitle: "localhost:3000", icon: server, parent: local
- id: gcp, label: "GCP example-prod-3c4d", style: gcp, type: container
- id: iap, label: "IAP", subtitle: "35.235.240.0/20", icon: iap, parent: gcp
- id: dns, label: "Cloud DNS", subtitle: "managed zone", icon: dns, parent: gcp
- id: lb, label: "HTTPS load balancer", subtitle: "managed certificate", icon: lb, parent: gcp
- id: vpc, label: "VPC (europe-west1)", style: vpc, type: container, parent: gcp
- id: vm, label: "my-vm", subtitle: "no public IP", icon: compute, parent: vpc
- id: relay, label: "Relay VM", subtitle: "e2-micro, no public IP", icon: compute, parent: vpc
edges:
- from: client, to: cli, label: "localhost:15432"
- from: cli, to: iap, label: "IAP tunnel"
- from: iap, to: vm, label: "TCP 5432"
- from: visitor, to: dns, label: "DNS lookup"
- from: dns, to: lb, label: "A record"
- from: lb, to: relay, label: "named port"
- from: relay, to: svc, label: "reverse SSH", style: dashed
:::
