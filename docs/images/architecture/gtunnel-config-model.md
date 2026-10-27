---
title: "The account and config model"
subtitle: "A tunnel is its name and its account, so kubernetes exists once per account"
date: "2026-10-27"
team: "gtunnel"
audience: "Engineering"
classification: "Public"
tags: [architecture]
---

## The account and config model

:::diagram-architecture "The account and config model"
nodes:
- id: local, label: "Your laptop", style: external, type: container
- id: tun, label: "tunnels.toml", subtitle: "CONFIG/tunnels.toml", icon: storage, parent: local
- id: t1, label: "dev / kubernetes", subtitle: "name + account_name", icon: service, parent: local
- id: t2, label: "prod / kubernetes", subtitle: "name + account_name", icon: service, parent: local
- id: f1, label: "dev artifacts", style: namespace, type: container, parent: local
- id: f1_plist, label: "launchd plist", subtitle: "com.gtunnel.dev.kubernetes", icon: service, parent: f1
- id: f1_log, label: "Tunnel log", subtitle: "logs/dev-kubernetes.log", icon: logging, parent: f1
- id: f1_port, label: "Local port", subtitle: "localhost:8443", icon: server, parent: f1
- id: f2, label: "prod artifacts", style: namespace, type: container, parent: local
- id: f2_plist, label: "launchd plist", subtitle: "com.gtunnel.prod.kubernetes", icon: service, parent: f2
- id: f2_log, label: "Tunnel log", subtitle: "logs/prod-kubernetes.log", icon: logging, parent: f2
- id: f2_port, label: "Local port", subtitle: "localhost:8444", icon: server, parent: f2
- id: cfg, label: "config.toml", subtitle: "CONFIG/config.toml", icon: storage, parent: local
- id: accts, label: "accounts", subtitle: "gcloud config, project, zone", icon: users, parent: local
- id: def, label: "default_account", subtitle: "overridden by --account", icon: users, parent: local
- id: relay, label: "relay, per account", subtitle: "VM, zone, load balancer", icon: compute, parent: local
- id: doms, label: "domains, per account", subtitle: "set by domains add", icon: dns, parent: local
edges:
- from: cfg, to: accts
- from: cfg, to: def
- from: accts, to: relay
- from: accts, to: doms
- from: tun, to: t1
- from: tun, to: t2
- from: t1, to: f1
- from: t2, to: f2
:::
