---
title: "Several services, one relay"
subtitle: "Each published service is its own supervised reverse-SSH session"
date: "2026-10-29"
team: "gtunnel"
audience: "Engineering"
classification: "Public"
tags: [architecture, gcp]
---

## Several services, one relay

:::diagram-architecture "Several services, one relay"
nodes:
- id: net, label: "Internet", style: external, type: container
- id: visitor, label: "Visitors", subtitle: "web, api, docs", icon: users, parent: net
- id: local, label: "Your laptop", style: external, type: container
- id: web, label: "web", subtitle: "localhost:3000", icon: server, parent: local
- id: api, label: "api", subtitle: "localhost:8000", icon: server, parent: local
- id: docs, label: "docs", subtitle: "localhost:4000", icon: server, parent: local
- id: sup, label: "gtunnel supervisors", subtitle: "one per service", icon: job, parent: local
- id: wd, label: "Machine watchdog", subtitle: "one pass every 30s", icon: scheduler, parent: local
- id: tui, label: "TUI probes", subtitle: "every 5s", icon: monitoring, parent: local
- id: gcp, label: "GCP example-prod-3c4d", style: gcp, type: container
- id: dns, label: "Cloud DNS", subtitle: "one A record per service", icon: dns, parent: gcp
- id: lb, label: "Global HTTPS LB", subtitle: "host rule per service", icon: lb, parent: gcp
- id: iap, label: "IAP", subtitle: "carries ssh to the relay", icon: iap, parent: gcp
- id: vpc, label: "VPC (europe-west1)", style: vpc, type: container, parent: gcp
- id: relay, label: "Relay instance", subtitle: "one port per service", icon: compute, parent: vpc
edges:
- from: visitor, to: lb, label: "HTTPS request"
- from: dns, to: lb, label: "A records", style: dashed
- from: lb, to: relay, label: "one backend each"
- from: web, to: iap, label: "ssh -R session"
- from: api, to: iap
- from: docs, to: iap
- from: iap, to: relay, label: "ssh, port 22"
- from: sup, to: web, label: "supervises", style: dashed
- from: sup, to: api, label: "supervises", style: dashed
- from: sup, to: docs, label: "supervises", style: dashed
- from: wd, to: sup, label: "probes", style: dashed
- from: tui, to: lb, label: "polls", style: dashed
:::
