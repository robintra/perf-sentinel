# Incident runbook

Operational guide for perf-sentinel in production. Each section is self-contained: start with the symptom that matches yours, work the **first checks** list, then escalate.

If you are setting up perf-sentinel for the first time, see [INTEGRATION.md](INTEGRATION.md). For HTTP API references, see [QUERY-API.md](QUERY-API.md). For configuration options, see [CONFIGURATION.md](CONFIGURATION.md). For the list of what the daemon does *not* guarantee, see [LIMITATIONS.md](LIMITATIONS.md).

## Contents

- [Diagnostic cheat sheet](#diagnostic-cheat-sheet): commands to run first in any incident
- [Analyzing a trace older than the live window](#analyzing-a-trace-older-than-the-live-window): post-mortem workflow
- [Daemon running but not reachable from clients](#daemon-running-but-not-reachable-from-clients)
- [No traces ingested](#no-traces-ingested)
- [Sudden drop in ingestion volume](#sudden-drop-in-ingestion-volume)
- [Spike in critical findings](#spike-in-critical-findings)
- [Daemon memory pressure or OOM](#daemon-memory-pressure-or-oom)
- [CI quality gate failing unexpectedly](#ci-quality-gate-failing-unexpectedly)
- [`perf-sentinel tempo` returns 404 or times out](#perf-sentinel-tempo-returns-404-or-times-out)
- [Exemplars missing in Grafana](#exemplars-missing-in-grafana)
- [Energy scraper stuck](#energy-scraper-stuck)
- [`/api/correlations` returns empty](#apicorrelations-returns-empty)
- [`/api/export/report` returns 503 or an empty report](#apiexportreport-returns-503-or-an-empty-report)
- [Daemon crash or restart](#daemon-crash-or-restart)
- [Applying config changes](#applying-config-changes)

---

## Diagnostic cheat sheet

Run these first regardless of the symptom. They give the 10-second picture of daemon state.

```bash
# Is the daemon alive? HTTP 200 with a metrics body means yes.
curl -sf http://perf-sentinel:4318/metrics | head -n 20

# Status summary: uptime, active traces, stored findings, version
curl -s http://perf-sentinel:4318/api/status | jq .

# Ingestion health at a glance
curl -s http://perf-sentinel:4318/metrics \
  | grep -E '^perf_sentinel_(events|traces|active)_'

# Recent critical findings
curl -s 'http://perf-sentinel:4318/api/findings?severity=critical&limit=20' \
  | jq '.[].finding | {finding_type, service, trace_id}'
```

Daemon logs with targeted verbosity (the daemon uses the standard `RUST_LOG` env var):

```bash
RUST_LOG=sentinel_core::daemon=info     # lifecycle, bind addresses, shutdown
RUST_LOG=sentinel_core::ingest=debug    # OTLP receive path, dropped events
RUST_LOG=sentinel_core::detect=debug    # detection pipeline
RUST_LOG=sentinel_core::score=debug     # green scoring, energy scrapers
```

For Kubernetes probes, use the dedicated `GET /health` endpoint (always exposed, independent of `[daemon] api_enabled`), which returns `200 OK` with `{"status":"ok","version":"..."}`. Lighter than `/metrics` and guaranteed lock-free. There is no separate `/ready` endpoint: the daemon accepts ingestion from the first tick, so liveness and readiness collapse into one probe.

---

## Analyzing a trace older than the live window

**Why you need this.** The daemon keeps traces in memory for **30 seconds** (`trace_ttl_ms`, default). Once evicted:

- `GET /api/explain/{trace_id}` returns `{"error": "trace not found in daemon memory"}`
- `GET /api/findings/{trace_id}` still returns findings (retained in the ring buffer up to `max_retained_findings = 10000`), but **the spans themselves are gone**, so no explain tree can be rebuilt from the daemon alone.

For anything older, the source of truth is your trace backend (typically Grafana Tempo).

**The four-step workflow.**

```
 1. Alert fires     →  Grafana panel on perf_sentinel_findings_total spikes
 2. Click exemplar  →  Grafana opens the trace in Tempo via the `trace_id` label
 3. Copy trace_id   →  from the Tempo view or the alert payload
 4. Replay          →  perf-sentinel tempo --endpoint <url> --trace-id <id>
```

Step 4 feeds the historical trace through the same `normalize → correlate → detect → score → explain` pipeline the daemon uses. You get the same findings and explain tree, only now on a trace from the past.

**Common invocations.**

```bash
# Explain a specific trace
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123def456

# Sweep a service over a window when you don't have a trace_id yet
perf-sentinel tempo --endpoint http://tempo:3200 --service order-svc --lookback 2h

# Post-mortem artifact for a ticket or PR
perf-sentinel tempo --endpoint http://tempo:3200 --trace-id abc123 --format json > incident.json
```

SARIF output (`--format sarif`) is supported if your incident process uses GitHub Code Scanning.

**Fallback: Tempo unavailable.** If Tempo is not reachable but you have a dump from another source (Jaeger/Zipkin export, archived S3 bucket, OTLP capture), pass the file directly:

```bash
perf-sentinel explain --input traces-dump.json --trace-id abc123def456
perf-sentinel analyze --input traces-dump.json
```

**What will NOT work.**

| Attempt                                             | Why it fails                                 |
|-----------------------------------------------------|----------------------------------------------|
| `curl /api/explain/<trace_id>` on the live daemon   | Trace evicted after 30 s                     |
| `curl /api/findings` to reconstruct an explain tree | The findings store keeps findings, not spans |
| Waiting for the daemon to "resurface" the trace     | No persistence, no replay endpoint           |
| Restarting the daemon to recover state              | Nothing is persisted to disk                 |

**Prerequisites.**

- **Tempo retention covers the incident window.** Default `block_retention` is 14 days but varies by deployment.
- **Sampling.** If the trace was dropped at ingestion by head- or tail-based sampling, it is gone from Tempo too. Consider 100 % sampling on error traces.
- **`trace_id` propagation.** Alerts and logs must carry the label. OpenMetrics exemplars on `perf_sentinel_findings_total` and `perf_sentinel_io_waste_ratio` are the easiest source.

**Optional: widen the live window.** If post-mortem inside the TTL is frequent, trade RAM for context:

```toml
[daemon]
max_active_traces     = 50000    # up to 1_000_000 hard cap
trace_ttl_ms          = 300000   # 5 minutes instead of 30 seconds
max_retained_findings = 50000
```

---

## Daemon running but not reachable from clients

**Symptom.** The daemon process is alive (container up, systemd unit active, logs show `Starting daemon: gRPC=...:4317, HTTP=...:4318`) but `curl http://<host>:4318/health` from outside the process times out or connection-refuses.

**First checks.**

```bash
# From inside the container / pod / host running the daemon (should always work):
curl -sf http://localhost:4318/health

# From where you actually want to reach it (this is the one that fails):
curl -v http://<host>:4318/health

# The bind address is logged explicitly at startup:
docker logs perf-sentinel 2>&1 | grep 'Starting daemon'
# Expect gRPC=0.0.0.0:4317 for external reach. Anything with 127.0.0.1 is
# loopback-only and will refuse connections from outside the process.
```

**Likely causes.**

1. **Daemon bound to `127.0.0.1` (the default).** The listener binds to the loopback interface for security. Inside a container, loopback is reachable only from *within* that same container, so `docker run -p 4318:4318` publishes a port at the host level but the in-container listener does not accept the forwarded connection. Same pattern on a VM accessed over SSH port-forward or on a Kubernetes pod behind a ClusterIP Service.
2. **`--network host` combined with `-p` flags.** In host network mode, the container shares the host's network namespace; `-p` flags are ignored and Docker emits `WARNING: Published ports are discarded when using host network mode`. The daemon is reachable only on whatever IP its config binds to.
3. **Port mapping reversed or incomplete.** `docker ps --format '{{.Ports}}'` shows the effective mapping. Expected pattern on a local dev run: `0.0.0.0:4317-4318->4317-4318/tcp`.
4. **Host firewall, NetworkPolicy, or cloud Security Group dropping the traffic.** The in-container `curl` succeeds but the external one times out. If the bind address is `0.0.0.0` and the daemon log shows no error, the delta is environment-side.

**Fix.**

- Cause (1): launch with `watch --listen-address 0.0.0.0`, or set `[daemon] listen_address = "0.0.0.0"` in `.perf-sentinel.toml`. The daemon will emit a non-loopback warning on startup, which is expected; gate access with a reverse proxy or NetworkPolicy in shared environments. See the Docker quickstart in [README.md](../README.md) and the sidecar/collector topologies in [INTEGRATION.md](INTEGRATION.md).
- Cause (2): drop the `-p` flags when using `--network host` (they are ignored) and ensure the daemon listens on `0.0.0.0`. Or switch back to the default bridge network + explicit `-p`.
- Cause (3): recreate the container with the correct `-p HOST:CONTAINER` ordering.
- Cause (4): compare `curl` from inside the network namespace (succeeds) with the external one (fails). If the delta is infrastructure, surface the blocking rule to the infra owner.

---

## No traces ingested

**Symptom.** `perf_sentinel_events_processed_total` and `perf_sentinel_traces_analyzed_total` are flat at zero. `/api/status` reports `active_traces: 0`.

**First checks.**

```bash
# Is the daemon listening on the expected ports?
kubectl logs deploy/perf-sentinel | grep -i "listening on"
# Expected: "OTLP gRPC listening on 0.0.0.0:4317"
#           "OTLP HTTP listening on 0.0.0.0:4318"

# From inside a service container, can you reach the daemon?
curl -sf http://perf-sentinel:4318/metrics
```

**Likely causes, in order.**

1. **Bind address.** The daemon defaults to `127.0.0.1`, unreachable from other containers. Set `listen_address = "0.0.0.0"` in `.perf-sentinel.toml` and restart.
2. **Protocol mismatch.** The OTel Java Agent defaults to gRPC on port 4317. Confirm `OTEL_EXPORTER_OTLP_PROTOCOL` matches the port your service targets: `grpc` → 4317, `http/protobuf` → 4318.
3. **Network policy.** A Kubernetes `NetworkPolicy` or security group may block cross-namespace traffic. Temporarily disable it or allow the service → daemon path explicitly.
4. **Service not instrumented.** Verify `OTEL_SDK_DISABLED=false` and that the service is producing spans (most OTel SDKs have internal counters or debug logs).
5. **OTLP endpoint URL typo.** `OTEL_EXPORTER_OTLP_ENDPOINT` should be `http://<host>:4318`. No `/v1/traces` suffix, the SDK appends it.

**Sanity check.** After a fix, drive one request through an instrumented service and watch:

```bash
watch -n 1 'curl -s http://perf-sentinel:4318/metrics | grep events_processed_total'
```

The counter should tick up within seconds.

---

## Sudden drop in ingestion volume

**Symptom.** `rate(perf_sentinel_events_processed_total[5m])` falls off a cliff or drops to zero while the daemon stays up (uptime keeps growing).

**First checks.**

```bash
# Confirm daemon is still alive, rules out a crash
curl -s http://perf-sentinel:4318/api/status | jq '{uptime_seconds, active_traces}'
```

**Likely causes.**

1. **Upstream traffic dropped.** Real traffic to your services fell; perf-sentinel is faithfully reporting reality. Cross-check with your load balancer or HTTP metrics.
2. **OTel collector down.** If a central collector sits between services and perf-sentinel, check the collector's own health and receive metrics first.
3. **Sampling change.** A config bump reduced the sampling rate. Audit recent commits in your OTel config repo.
4. **Daemon backpressure.** If the OTLP receive channel is full, events drop silently. Look for `channel full` warnings in logs with `RUST_LOG=sentinel_core::ingest=debug`. Common triggers: detection pipeline stalled on a pathological trace; `max_active_traces` too low for current throughput.

Work top-to-bottom by elimination. Cases 1 and 2 account for the vast majority.

---

## Spike in critical findings

**Symptom.** Alert fires on `perf_sentinel_findings_total{severity="critical"}` rate.

**Triage workflow.**

1. **Group by service and type.**

   ```bash
   curl -s 'http://perf-sentinel:4318/api/findings?severity=critical&limit=200' \
     | jq '[.[].finding | {finding_type, service}]
          | group_by(.service, .finding_type)
          | map({key: "\(.[0].service)/\(.[0].finding_type)", count: length})
          | sort_by(-.count)'
   ```

2. **Grab an exemplar `trace_id`** for each top pattern. In Grafana, the ◆ on the metric is clickable; from the command line:

   ```bash
   curl -s http://perf-sentinel:4318/metrics \
     | grep -E 'findings_total|io_waste_ratio'
   # Lines end with "# {trace_id=\"...\"}", copy that id
   ```

3. **Explain the trace** while it's still in the 30-second live window:

   ```bash
   curl -s http://perf-sentinel:4318/api/explain/<trace_id> | jq .
   ```

   If evicted, pivot to [the post-mortem workflow](#analyzing-a-trace-older-than-the-live-window).

4. **Correlate across services** if the incident spans multiple teams:

   ```bash
   curl -s http://perf-sentinel:4318/api/correlations | jq 'sort_by(-.confidence)[:10]'
   ```

**Common root causes.**

- **N+1 SQL:** ORM lazy loading; a recent feature iterating a collection without `JOIN FETCH` / `selectinload` / `Include`.
- **Pool saturation:** connection pool undersized, or a downstream dependency slowed down.
- **Slow query:** missing index; a data-volume threshold crossed (what ran in 50 ms at 10 k rows now runs in 2 s at 10 M).

---

## Daemon memory pressure or OOM

**Symptom.** RSS grows over time; Kubernetes OOMKill; `active_traces` or `stored_findings` hovering near their caps.

**First checks.**

```bash
curl -s http://perf-sentinel:4318/api/status | jq '{active_traces, stored_findings, uptime_seconds}'
# Compare against config's max_active_traces (default 10000) and max_retained_findings (default 10000).
```

**Likely causes.**

1. **Traffic exceeds defaults.** 10 000 active traces is sized for moderate load. High-throughput services fill it faster than eviction keeps up.
2. **Widened TTL.** If you raised `trace_ttl_ms` for post-mortem convenience, every trace lives longer in memory.
3. **Pathological traces.** A single trace with thousands of spans eats RAM. `max_events_per_trace` (default 1000) caps this; confirm it hasn't been raised.
4. **Correlator growth.** `[daemon.correlation] max_tracked_pairs` (default 10 000) bounds the cross-trace graph. Raising it multiplies memory by the pair count.
5. **Findings store inflated** by a runaway detection loop. Rare but worth checking `stored_findings` vs `max_retained_findings`.

**Fix.**

```toml
[daemon]
max_active_traces     = 5000     # smaller window
trace_ttl_ms          = 30000    # back to default
api_enabled           = false    # disable query API if unused
max_retained_findings = 0        # short-circuits the findings ring buffer

[daemon.correlation]
enabled = false                  # skip the correlator for single-service daemons
```

Setting `max_retained_findings = 0` is the most effective RAM-reclaim lever when the query API isn't consumed. See [LIMITATIONS.md](LIMITATIONS.md) § "Memory is not reclaimed by `api_enabled = false` alone".

Restart the daemon to apply. **No hot reload**, see [Applying config changes](#applying-config-changes).

---

## CI quality gate failing unexpectedly

**Symptom.** `perf-sentinel analyze --ci` or `perf-sentinel tempo --ci` exits with code 1. Build red.

**First checks.**

The JSON output contains a structured `quality_gate` block:

```bash
perf-sentinel analyze --ci --input traces.json --format json \
  | jq '.quality_gate.rules[] | select(.passed == false)'
```

Example output:

```json
{ "rule": "n_plus_one_sql_critical_max", "threshold": 0, "actual": 2, "passed": false }
```

**Likely causes.**

1. **Legitimate regression.** A recent change introduced new N+1s or widened the waste ratio. Inspect `findings[]` in the same JSON: `source_endpoint` locates the code path; `pattern.template` shows the normalized SQL/HTTP call; `pattern.occurrences` tells you how bad.
2. **Threshold too tight.** `.perf-sentinel.toml` may have zero-tolerance limits that fail on any pre-existing finding. For brownfield projects, consider a ratcheting baseline (tighten over time rather than all at once).
3. **Test data grew.** A larger dataset in integration tests can cross a detection threshold (a 5-occurrence N+1 only fires above a certain iteration count).

**Fix.** Adjust either the code or the threshold, not both under pressure. If the finding is real, fix the code. If the threshold is miscalibrated, update `.perf-sentinel.toml` and commit the change so it's reviewable.

> **Note.** There are no per-service detection thresholds today; `[detection]` values apply globally across all services in the trace file.

---

## `perf-sentinel tempo` returns 404 or times out

**Symptom.** Either every invocation fails with `Tempo returned HTTP 404 for https://.../api/search?...`, or the search step succeeds but the per-trace fetch loop finishes with `Tempo fetch completed with failures counts={"timeout": N}` and returns a partial (or empty) result.

**First checks.**

```bash
# Confirm the endpoint is actually a Tempo query-frontend, not Grafana or
# an internal Tempo component. 200 = good, 404 = wrong endpoint.
curl -s -o /dev/null -w 'HTTP %{http_code}\n' \
  '<your-endpoint>/api/search?limit=1'

# On Tempo side, watch the query-frontend load
kubectl logs -n observability deploy/tempo-query-frontend --tail=50 \
  | grep -E 'error|timeout|queue'
```

**Likely causes.**

1. **Wrong component in a microservices deployment.** In `tempo-distributed` Helm deployments, the HTTP query API is served exclusively by `tempo-query-frontend`. Pointing `--endpoint` at `tempo-querier` (an internal worker, no public API) or `tempo-ingester` (write path only) returns 404 on every `/api/search`. The 404 message emitted by perf-sentinel now includes the failing URL so the misconfiguration is visible at a glance.
2. **Endpoint pointing at Grafana instead of Tempo.** Grafana defaults to port 3000, Tempo HTTP API to 3200. `http://grafana:3000/api/search` has no backing route, returns 404.
3. **Reverse-proxy path prefix omitted.** If Tempo sits behind ingress with a path prefix (e.g. `https://observability.example.com/tempo/...`), `--endpoint` must include the prefix.
4. **Tempo degraded under fetch load.** Search succeeded but per-trace fetches time out. Common triggers: long `--lookback` (24 h on a large service), under-provisioned `tempo-query-frontend` replicas, `max_concurrent_queries` hit, ingester resource limits (OOM-killed ingesters produce cascading fetch failures).

**Fix.**

- Causes (1), (2), (3): point `--endpoint` at the actual query-frontend URL, validated by the `curl` above.
- Cause (4): on the perf-sentinel side, narrow `--lookback` (start at 1 h, widen progressively) or fall back to `--trace-id <id>` for a single-trace replay. On the Tempo side, scale `tempo-query-frontend` horizontally, raise `max_concurrent_queries`, and check ingester memory/CPU caps.

Perf-sentinel caps in-flight fetches at 16 concurrent by default, so the client is not itself flooding Tempo. If Tempo still collapses under a 100-trace run, capacity is the bottleneck, not the client. Hitting Ctrl-C during a long run now returns a partial result with the already-completed traces (see [LIMITATIONS.md](LIMITATIONS.md) § "Tempo ingestion"); the CLI surfaces `Tempo fetch was interrupted by Ctrl-C before any trace completed` when zero traces had completed, distinct from the generic `NoTracesFound`.

---

## Exemplars missing in Grafana

**Symptom.** Panels render metric values but the ◆ exemplar marker is absent, or clicking it doesn't jump to Tempo.

**First checks.**

```bash
# Raw metrics: look for "# {trace_id=\"...\"}" at line ends
curl -s http://perf-sentinel:4318/metrics \
  | grep -E 'findings_total|io_waste_ratio'
```

If the annotations are present in the raw output but Grafana doesn't render them, it's a Grafana or Prometheus configuration issue. If absent, perf-sentinel hasn't recorded any exemplar yet.

**Likely causes.**

1. **No findings yet.** Exemplars are only set on detection. A zero-findings daemon has none. Drive traffic through a path that triggers an N+1 or slow query.
2. **Prometheus exemplar storage not enabled.** Prometheus must be started with `--enable-feature=exemplar-storage`. Verify on the Prometheus flags page.
3. **Grafana datasource not linked to Tempo.** In Grafana → Connections → Prometheus datasource → Exemplars, set an exemplar with `datasourceUid` pointing to your Tempo datasource and `labelName: trace_id`.
4. **`trace_id` sanitized away.** perf-sentinel strips exemplar values to `[a-zA-Z0-9_-]` and truncates to 64 chars. Unusual trace ID formats (UUIDs with braces, custom encodings) may be mangled. See `sanitize_exemplar_value` in `report/metrics.rs`.

---

## Energy scraper stuck

**Symptom.** `perf_sentinel_scaphandre_last_scrape_age_seconds` or `perf_sentinel_cloud_energy_last_scrape_age_seconds` grows monotonically past the configured scrape interval. Healthy scrapers reset this gauge near zero after each successful scrape.

**First checks.**

```bash
curl -s http://perf-sentinel:4318/metrics | grep scrape_age_seconds
```

Enable scoring logs to see the actual failure:

```bash
RUST_LOG=sentinel_core::score=debug
# Look for "scaphandre scrape failed" or "cloud_energy scrape failed"
```

**Likely causes.**

1. **Scaphandre container permissions.** RAPL counters require `CAP_SYS_RAWIO`, privileged mode, or a hostPath mount of `/sys/class/powercap`. Without these, scrapes fail at the privilege layer.
2. **Endpoint unreachable.** Check the URL in `[green.scaphandre] endpoint`. Network between perf-sentinel and the Scaphandre exporter must be open.
3. **Cloud energy API down or rate-limited.** If using Electricity Maps or a cloud-provider API, check its status and your API quota.
4. **Service name mismatch.** `[green.cloud.services.<name>]` keys must match the `service.name` attribute on incoming spans. No match, no per-service attribution.

**Impact.** The daemon falls back to the I/O proxy energy model. CO₂ figures remain directional but lose their measured-energy precision. Not a hot incident; fix at your next maintenance window unless the accuracy matters for a specific report.

---

## `/api/correlations` returns empty

**Symptom.** Cross-trace correlation panels are empty even though multiple services are producing findings.

**First checks.**

```bash
curl -s http://perf-sentinel:4318/api/correlations | jq 'length'
# 0 means no correlations passed the thresholds
```

**Likely causes.**

1. **Correlator disabled.** The default is `[daemon.correlation] enabled = false`. Enable it.
2. **Thresholds too strict.** Defaults:
   - `min_co_occurrences = 5`: need 5 joint incidents before a pair is considered
   - `min_confidence = 0.7`: 70 % correlation confidence
   - `lag_threshold_ms = 5000`: 5-second window between cause and effect

   Short bursts of traffic rarely accumulate 5 co-occurrences. Lower for dev/staging, keep conservative in prod.
3. **Services legitimately independent.** Healthy decoupled services produce no correlations. Absence is not always a bug.

**Fix.**

```toml
[daemon.correlation]
enabled            = true
min_co_occurrences = 3
min_confidence     = 0.6
lag_threshold_ms   = 10000
max_tracked_pairs  = 20000
```

Restart the daemon to apply.

---

## `/api/export/report` returns 503 or an empty report

**Symptom.** Piping the daemon into the HTML dashboard fails with HTTP 503 or produces a dashboard with zero findings on a daemon that is clearly running.

```bash
curl -s http://perf-sentinel:4318/api/export/report | perf-sentinel report --input - --output /tmp/report.html
# HTTP 503: {"error": "daemon has not yet processed any events"}
```

**Likely causes.**

1. **Cold start.** The endpoint returns 503 until `events_processed > 0`, on purpose: rendering a dashboard with zero counters on a daemon that has not yet seen its first OTLP batch would be misleading. Wait for the first batch to land, then retry. `GET /api/status` shows the live `events_processed` counter.
2. **`api_enabled = false`.** If the config disables the query API, `/api/export/report` is not mounted and `curl` returns a 404, not a 503. Re-enable `[daemon] api_enabled = true`.
3. **Empty findings store, not cold start.** On a long-running daemon that has processed events but has no findings in the ring buffer (clean traffic, or `max_retained_findings = 0`), the endpoint returns 200 with an empty `findings` array. The resulting dashboard shows a "No findings" empty state, which is correct.

**Operational note.** The snapshot is not atomic across `findings` and `correlations`: the two collections can be one batch apart (findings from generation N, correlations from N+1). For a post-mortem dashboard this is acceptable. If you need strict consistency, use `analyze --input traces.json` on a captured trace file instead.

---

## Daemon crash or restart

**Symptom.** The daemon process exited unexpectedly (kernel OOM, panic, pod eviction, deploy rollout).

**What is lost.**

- All traces in the sliding window (up to `max_active_traces`).
- All retained findings (up to `max_retained_findings`).
- Cross-trace correlation state.
- Uptime counter resets.

**What survives.**

- Nothing from the daemon itself. There is no disk persistence.
- Prometheus retains the metrics it already scraped (historical counters are safe).
- Tempo retains the traces, assuming you also send them there.

**Recovery.**

1. Start a new daemon with the same config.
2. Wait for OTel collectors / SDKs to reconnect. OTel clients retry with exponential backoff. Expect up to ~60 seconds before ingestion resumes fully.
3. For incidents that occurred *during* the downtime, use [the post-mortem workflow](#analyzing-a-trace-older-than-the-live-window) against Tempo.

**Prevention.**

- Kubernetes `restartPolicy: Always` + memory limit headroom above observed peak RSS.
- Alert on `perf_sentinel_active_traces` approaching `max_active_traces`. Rising pressure often precedes OOM.
- For HA, run multiple replicas behind a load balancer. Each replica has independent state (no cross-replica correlation), but ingestion becomes redundant against single-instance failure.

---

## Applying config changes

**The daemon does not hot-reload `.perf-sentinel.toml`.** Any config edit requires a restart:

```bash
# Kubernetes
kubectl rollout restart deployment/perf-sentinel

# systemd
systemctl restart perf-sentinel

# Docker
docker restart perf-sentinel
```

Expect a brief interruption in ingestion (seconds to a minute) driven by OTel SDK retry behavior. For non-urgent tuning, piggyback on a normal deployment window.

**Validate before rolling out.** The daemon parses TOML at startup and exits with a clear error on malformed input. Smoke-test the candidate config in a throwaway daemon first:

```bash
perf-sentinel watch --config /path/to/candidate-config.toml
# Exits immediately on parse error and prints the offending line.
```

Once it starts cleanly, roll it out to production.

---

## See also

- [LIMITATIONS.md](LIMITATIONS.md): what the daemon does *not* persist or guarantee.
- [QUERY-API.md](QUERY-API.md): reference for `/api/findings`, `/api/explain`, `/api/correlations`, `/api/status`.
- [INTEGRATION.md](INTEGRATION.md): end-to-end setup, Tempo integration, per-language OTLP wiring.
- [CONFIGURATION.md](CONFIGURATION.md): full `[daemon]`, `[detection]`, `[green]`, `[daemon.correlation]` reference.
