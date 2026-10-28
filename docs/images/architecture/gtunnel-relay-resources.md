---
title: "What relay up creates"
subtitle: "Per account once, per domain, per tunnel, and what each command creates"
date: "2026-10-27"
team: "gtunnel"
audience: "Engineering"
classification: "Public"
tags: [architecture, gcp]
---

## What relay up creates

:::diagram-architecture "What relay up creates"
nodes:
- id: local, label: "Your laptop", style: external, type: container
- id: up, label: "gtunnel relay up", subtitle: "once per account", icon: laptop, parent: local
- id: dom, label: "gtunnel domains add", subtitle: "once per domain", icon: laptop, parent: local
- id: exp, label: "gtunnel expose", subtitle: "or run --public", icon: laptop, parent: local
- id: gcp, label: "GCP example-prod-3c4d", style: gcp, type: container
- id: acct, label: "Per account, once", style: namespace, type: container, parent: gcp
- id: front, label: "HTTPS front end", subtitle: "address, rule, HTTPS proxy", icon: lb, parent: acct
- id: urlmap, label: "URL map", subtitle: "default backend, health check", icon: lb, parent: acct
- id: vm, label: "Relay instance", subtitle: "and its instance group", icon: compute, parent: acct
- id: fw, label: "Firewall rules", subtitle: "gt-ACCT-iap-ssh, gt-ACCT-lb-hc", icon: armor, parent: acct
- id: cm, label: "Certificate map", subtitle: "gt-ACCT-certs", icon: certmanager, parent: acct
- id: perdom, label: "Per domain", style: namespace, type: container, parent: gcp
- id: cert, label: "Certificate", subtitle: "and DNS authorization", icon: certmanager, parent: perdom
- id: entries, label: "Cert-map entries", subtitle: "apex and wildcard", icon: certmanager, parent: perdom
- id: pertun, label: "Per tunnel", style: namespace, type: container, parent: gcp
- id: backend, label: "Backend service", subtitle: "and named port", icon: lb, parent: pertun
- id: host, label: "URL-map host rule", subtitle: "the hostname", icon: lb, parent: pertun
- id: arec, label: "Cloud DNS A record", subtitle: "TTL 60", icon: dns, parent: pertun
edges:
- from: up, to: acct, label: "creates"
- from: dom, to: perdom, label: "creates"
- from: exp, to: pertun, label: "creates"
- from: entries, to: cm, label: "added to", style: dashed
- from: host, to: urlmap, label: "added to", style: dashed
:::
