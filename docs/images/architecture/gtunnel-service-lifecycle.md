---
title: "Service lifecycle"
subtitle: "One shape on both platforms, only the service manager differs"
date: "2026-10-27"
team: "gtunnel"
audience: "Engineering"
classification: "Public"
tags: [architecture]
---

## Service lifecycle

:::diagram-architecture "Service lifecycle"
nodes:
- id: local, label: "Your laptop", style: external, type: container
- id: add, label: "gtunnel add", subtitle: "new tunnel", icon: laptop, parent: local
- id: rec, label: "tunnels.toml", subtitle: "name, account, target, port", icon: storage, parent: local
- id: start, label: "gtunnel start", subtitle: "or s in the TUI", icon: laptop, parent: local
- id: write, label: "Write the definition", subtitle: "com.gtunnel.ACCOUNT.NAME", icon: storage, parent: local
- id: mac, label: "macOS", style: namespace, type: container, parent: local
- id: macload, label: "launchctl bootstrap", subtitle: "CONFIG/agents/LABEL.plist", icon: service, parent: mac
- id: la, label: "Starts at login", subtitle: "~/Library/LaunchAgents/LABEL.plist", icon: cronjob, parent: mac
- id: lin, label: "Linux", style: namespace, type: container, parent: local
- id: linload, label: "systemctl --user start", subtitle: "~/.config/systemd/user/LABEL.service", icon: service, parent: lin
- id: en, label: "systemctl --user enable", subtitle: "plus loginctl enable-linger", icon: cronjob, parent: lin
- id: proc, label: "gcloud", subtitle: "child process", icon: server, parent: local
- id: again, label: "Restart on failure", subtitle: "KeepAlive or RestartSec=5", icon: service, parent: local
- id: down, label: "Stays down", subtitle: "run/LABEL.auth, auth expired", icon: iam, parent: local
edges:
- from: add, to: rec
- from: rec, to: start
- from: start, to: write
- from: write, to: macload
- from: write, to: linload
- from: macload, to: proc
- from: linload, to: proc
- from: proc, to: again, label: "any other non-zero exit"
- from: proc, to: down, label: "credentials expired"
- from: down, to: start, label: "gtunnel start clears it", style: dashed
- from: again, to: proc
- from: macload, to: la, label: "A toggles auto-start", style: dashed
- from: linload, to: en, label: "A toggles auto-start", style: dashed
:::
