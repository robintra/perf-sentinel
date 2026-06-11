#!/usr/bin/env python3
"""Continuously seed a perf-sentinel daemon's JSON socket with varying
sub-batches of a fixture, so the `query monitor` Trends tab has moving
curves to plot: each batch closes a new scoring window with a different
energy total, and the findings store fills up over time.

Usage: demo-seed-loop.py <socket-path> <fixture.json> [period-secs] [bias-services]

The sampling is not uniform: a sinusoidal bias alternates between
batches dominated by `bias-services` (comma-separated, default
`checkout-svc,dashboard-svc`, which demo-monitor.tape maps to the
high-carbon us-east-1 and ap-southeast-2 regions) and batches dominated
by everything else (mapped to low-carbon regions). The regional mix of
each scoring window therefore shifts over time, the effective gCO2e/kWh
moves, and the monitor's carbon curve visibly decorrelates from its
energy curve, the way a live Electricity Maps intensity feed would.

Each kept event gets its trace id rewritten with a unique suffix, so
re-seeded traces never merge with already-flushed ones. Connection
errors are ignored: the daemon may still be starting, or already gone
(the tape teardown kills this loop).
"""

import json
import math
import random
import socket
import sys
import time


def main() -> None:
    sock_path = sys.argv[1]
    fixture = sys.argv[2]
    period = float(sys.argv[3]) if len(sys.argv) > 3 else 1.3
    bias_arg = sys.argv[4] if len(sys.argv) > 4 else "checkout-svc,dashboard-svc"
    bias_services = set(bias_arg.split(","))
    with open(fixture, encoding="utf-8") as f:
        events = json.load(f)
    i = 0
    while True:
        i += 1
        # 0.04..0.96 share for the biased (high-carbon) services, full
        # cycle roughly every 13 iterations (~16 s at the default
        # period), so a ~30 s monitor window shows two mix swings.
        dirty_share = 0.5 + 0.46 * math.sin(i / 2.0)
        batch = []
        for event in events:
            keep = dirty_share if event.get("service") in bias_services else 1.0 - dirty_share
            if random.random() < keep:
                event = dict(event)
                event["trace_id"] = f"{event['trace_id']}-seed{i}"
                batch.append(event)
        if len(batch) < 6:
            # Keep every window non-trivial at the cycle extremes, with a
            # small top-up so it barely dilutes the mix. Cap at the fixture
            # size so a tiny fixture cannot raise ValueError.
            for event in random.sample(events, min(6, len(events))):
                event = dict(event)
                event["trace_id"] = f"{event['trace_id']}-seed{i}-fill"
                batch.append(event)
        try:
            # `with` closes the socket on every path, including a mid-send
            # error, so a long-running loop cannot leak file descriptors.
            with socket.socket(socket.AF_UNIX) as s:
                s.connect(sock_path)
                s.sendall((json.dumps(batch) + "\n").encode())
                s.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        time.sleep(period)


if __name__ == "__main__":
    main()
