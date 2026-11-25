#!/usr/bin/env python3
"""Measure what running tunnels cost on this machine.

    scripts/measure-tunnels.py PORT [PORT ...]          every tunnel's processes and latency
    scripts/measure-tunnels.py --samples 50 PORT        more connections for the percentiles
    scripts/measure-tunnels.py --start NAME PORT        seconds from `gtunnel start` to a listening port

For each local port it finds the processes holding it (one `lsof` listing) and the
processes below them (one `ps` snapshot), and adds up their resident memory. It then
connects `--samples` times and reports how long the connect took. Nothing is sent over
a connection, but a connect to a direct tunnel is one IAP connection to the instance (and a
pre-auth sshd line on port 22), so the default of 20 samples is 20 of them. A jump tunnel's
connect opens neither.

It measures what this machine runs, not what IAP allows: Google publishes no limits
(docs/architecture.md, Limits). Run it against 1, 5 and 10 tunnels of each shape and
keep the output with the gtunnel and gcloud versions it came from.
"""
import argparse
import json
import socket
import statistics
import subprocess
import sys
import time


def run(argv):
    return subprocess.run(argv, capture_output=True, text=True, check=False).stdout


def holders(port):
    """Pids listening on the port."""
    out = run(["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-t"])
    return sorted({int(p) for p in out.split()})


def snapshot():
    """pid -> (ppid, rss in KiB, command), from one ps."""
    table = {}
    for line in run(["ps", "-axo", "pid=,ppid=,rss=,command="]).splitlines():
        parts = line.split(None, 3)
        if len(parts) == 4 and parts[0].isdigit():
            table[int(parts[0])] = (int(parts[1]), int(parts[2]), parts[3])
    return table


def tree(roots, table):
    """The roots and everything below them."""
    seen = set(roots)
    grew = True
    while grew:
        grew = False
        for pid, (ppid, _, _) in table.items():
            if ppid in seen and pid not in seen:
                seen.add(pid)
                grew = True
    return seen


def connect_times(port, samples):
    times = []
    for _ in range(samples):
        began = time.perf_counter()
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=3):
                times.append((time.perf_counter() - began) * 1000)
        except OSError:
            return times, True
    return times, False


def percentile(values, q):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(len(ordered) * q))]


def measure(port, samples, table):
    pids = tree(holders(port), table)
    row = {
        "port": port,
        "processes": len(pids),
        "rss_mib": round(sum(table[p][1] for p in pids if p in table) / 1024, 1),
        "commands": sorted({table[p][2].split()[0].rsplit("/", 1)[-1] for p in pids if p in table}),
    }
    times, refused = connect_times(port, samples)
    row["refused"] = refused
    if times:
        row["connect_ms"] = {
            "median": round(statistics.median(times), 2),
            "p95": round(percentile(times, 0.95), 2),
            "max": round(max(times), 2),
            "samples": len(times),
        }
    return row


def start_time(name, port, deadline=60.0):
    """Seconds from `gtunnel start NAME` returning to the port accepting a connection."""
    began = time.perf_counter()
    started = subprocess.run(["gtunnel", "start", name], capture_output=True, text=True, check=False)
    if started.returncode != 0:
        return {"name": name, "error": started.stderr.strip() or started.stdout.strip()}
    while time.perf_counter() - began < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return {"name": name, "port": port, "seconds": round(time.perf_counter() - began, 2)}
        except OSError:
            time.sleep(0.1)
    return {"name": name, "port": port, "error": f"not listening after {deadline:.0f}s"}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("ports", nargs="*", type=int, metavar="PORT")
    parser.add_argument("--samples", type=int, default=20)
    parser.add_argument("--start", metavar="NAME", help="start this tunnel and time it; give its PORT")
    args = parser.parse_args()
    if not args.ports:
        parser.error("give at least one local PORT")
    if args.start:
        print(json.dumps(start_time(args.start, args.ports[0]), indent=2))
        return 0
    table = snapshot()
    rows = [measure(port, args.samples, table) for port in args.ports]
    total = {
        "tunnels": len(rows),
        "processes": sum(r["processes"] for r in rows),
        "rss_mib": round(sum(r["rss_mib"] for r in rows), 1),
    }
    print(json.dumps({"tunnels": rows, "total": total}, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
