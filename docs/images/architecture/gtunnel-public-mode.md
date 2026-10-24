---
title: "Public mode"
subtitle: "A global HTTPS load balancer in front of a relay VM"
date: "2026-10-24"
team: "Platform Engineering"
audience: "Engineering"
classification: "Internal"
tags: [architecture, gcp]
---

## Public mode

:::diagram-architecture "Public mode"
nodes:
- id: net, label: "Internet", style: external, type: container
- id: visitor, label: "Visitors", subtitle: "web.example.com", icon: users, parent: net
- id: gcp, label: "GCP example-prod-3c4d", style: gcp, type: container
- id: dns, label: "Cloud DNS", subtitle: "A record to gt-prod-ip", icon: dns, parent: gcp
- id: lb, label: "Global HTTPS LB", subtitle: "managed cert, host rule", icon: lb, parent: gcp
- id: vpc, label: "VPC (europe-west1)", style: vpc, type: container, parent: gcp
- id: relay, label: "Relay VM", subtitle: "e2-micro, no public IP", icon: compute, parent: vpc
- id: local, label: "Your laptop", style: external, type: container
- id: svc, label: "Local service", subtitle: "localhost:3000", icon: server, parent: local
edges:
- from: visitor, to: lb, label: "HTTPS request"
- from: dns, to: lb, label: "A record", style: dashed
- from: lb, to: relay, label: "backend, port 30001"
- from: relay, to: svc, label: "reverse SSH", style: dashed
:::
