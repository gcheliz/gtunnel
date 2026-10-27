---
title: "Access and jump tunnels"
subtitle: "One hop to a VM, two hops to a host only the VM can reach"
date: "2026-10-27"
team: "gtunnel"
audience: "Engineering"
classification: "Public"
tags: [architecture, gcp]
---

## Access and jump tunnels

:::diagram-architecture "Access and jump tunnels"
nodes:
- id: local, label: "Your laptop", style: external, type: container
- id: client_a, label: "psql", subtitle: "localhost:15432", icon: server, parent: local
- id: client_b, label: "kubectl", subtitle: "localhost:8443", icon: server, parent: local
- id: gcp, label: "GCP example-dev-1a2b", style: gcp, type: container
- id: iap, label: "IAP", subtitle: "35.235.240.0/20", icon: iap, parent: gcp
- id: vpc, label: "VPC (europe-west1)", style: vpc, type: container, parent: gcp
- id: vm, label: "my-vm", subtitle: "port 5432, no public IP", icon: compute, parent: vpc
- id: bastion, label: "bastion", subtitle: "port 22, no public IP", icon: compute, parent: vpc
- id: host, label: "db.internal", subtitle: "no route from laptop", icon: server, parent: vpc
edges:
- from: client_a, to: iap, label: "start-iap-tunnel"
- from: iap, to: vm, label: "TCP 5432"
- from: client_b, to: iap, label: "ssh through IAP"
- from: iap, to: bastion, label: "SSH to port 22"
- from: bastion, to: host, label: "ssh -L forward"
:::
