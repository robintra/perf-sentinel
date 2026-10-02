# Security overview

This page gathers, for a security review, what Perf Sentinel and its companion service [PerfSentinelHub](https://github.com/robintra/PerfSentinelHub) expose, send, store and sign. Each statement gives the default and what is left to the operator, and points to the document that details it. Facts are checked against the engine at 0.25.4 and the Hub at 0.3.4.

Perf Sentinel is a single self-hosted binary that analyzes OpenTelemetry traces, in CI or as a daemon (`watch`). The Hub is an optional self-hosted service that keeps the history of findings across a fleet of daemons. Neither is a SaaS, and neither sends usage telemetry.

## At a glance

| Question | Perf Sentinel (engine) | PerfSentinelHub |
|---|---|---|
| Hosting | Your infrastructure, binary or container | Your infrastructure, one replica, SQLite |
| Inbound ports | OTLP gRPC `4317`, OTLP HTTP `4318`. The query API, `/metrics` and `/health` share `4318` | HTTP `8080` |
| Default bind | `127.0.0.1` for the binary, `0.0.0.0` in the Helm chart | All interfaces |
| TLS | Opt-in (`[daemon.tls]`), no client certificates | None in the process, terminate it at the ingress |
| Authentication | Ingest, `/metrics`, `/health` and the read endpoints are never authenticated. Ack writes need a key once one is set | The findings push needs a per-source key. The UI is open by default, with an optional OAuth2 sign-in |
| Outbound calls by default | None | A daily check of the latest releases on the GitHub API, one setting turns it off |
| Data at rest | The ack log, plus an opt-in NDJSON archive. Never raw spans | SQLite database, findings kept 180 days |
| Image | `FROM scratch`, UID 65534 | Chiseled Ubuntu, no shell, UID 1654 |
| Helm hardening | Non-root, read-only root filesystem, all capabilities dropped, `RuntimeDefault` seccomp, no service account token. NetworkPolicy template off by default | Non-root, read-only root filesystem, all capabilities dropped, `RuntimeDefault` seccomp. No NetworkPolicy template |
| Release integrity | Signed tags, SLSA build provenance (Build Level 2) and SPDX SBOM for binaries, Cosign-signed chart. Images unsigned | Signed tags, Cosign signature on every release artefact, GitHub provenance, SPDX SBOM, rebuilds compared byte for byte |
| Vulnerability reports | Private GitHub report, acknowledged within 72 hours | Private GitHub report, acknowledged within 3 business days |
| License | AGPL-3.0-only | AGPL-3.0-only |

## Network flows

### Inbound

The engine daemon listens on OTLP gRPC `4317` and OTLP HTTP `4318`. The query API (`/api/*`), `/metrics` and `/health` are served on the same `4318` listener, so a firewall rule or a NetworkPolicy cannot let traces in while keeping the query API closed. A local JSON socket (`/tmp/perf-sentinel.sock` by default) is created with mode `0600`. The `capture` command receives OTLP the same way, on `127.0.0.1` by default. The other commands listen on nothing.

The binary binds to `127.0.0.1` by default and logs a warning on any other address. The Helm chart sets `0.0.0.0`, which a pod needs behind its Service, and moves access control to the Service and to the chart's NetworkPolicy, which is off by default.

The Hub listens on plain HTTP `8080`. TLS is expected at the ingress.

### Outbound

The engine makes no outbound call unless the configuration or the command line names a destination:

| Destination | What triggers it |
|---|---|
| Energy sources: Alumet, Scaphandre, Kepler, Redfish BMC, cloud energy through Prometheus | A `[green.*]` section in the configuration |
| Electricity Maps (`api.electricitymaps.com`) | A `[green.electricity_maps]` section |
| Tempo, Jaeger, Victoria Traces | The `tempo` and `jaeger-query` commands |
| Prometheus | `pg-stat` and `mysql-stat` with `--prometheus` |
| PerfSentinelHub | `[daemon.hub_export] enabled = true` |
| A running daemon | The `query`, `ack` and TUI commands |
| An HTTPS URL | `verify-hash --url` |
| Local processes | `verify-hash` runs `cosign` and `gh`, `capture` runs the command you give it |

Reference data (carbon intensities, power tables) is compiled into the binary and never fetched at runtime.

The engine's HTTPS client trusts the bundled Mozilla root certificates only. It reads no proxy variable and accepts no private CA, so outbound HTTPS through a TLS-inspecting proxy fails. Plain HTTP to an internal endpoint is not affected.

The Hub calls its configured daemon sources (poll, ack relay, live view), and the engine subprocess it launches reaches the trace backend set in its configuration. With sign-in enabled, it calls the identity provider's token and userinfo endpoints. Once a day, it asks the GitHub releases API for the latest engine and Hub versions. This check is on by default, and `hub.updateCheck.enabled: false` turns it off for a cluster with no egress.

## Data handled and stored

**Normalization.** Spans are processed in memory, inside a streaming window (30 s TTL, 10,000 traces at most by default). SQL literals become `?`, numeric and UUID path segments become `{id}` and `{uuid}`, and the query string is dropped. A finding carries the resulting template and a count of distinct values, not the values. A few things stay verbatim, such as text between double quotes, SQL comments and non-numeric path segments: see [What stays verbatim in a template](LIMITATIONS.md#sql-tokenizer). A finding also carries the service name, the grouping attribute values, the code location and the trace id.

**What the daemon writes to disk.**
- The ack log, `acks.jsonl`, under the user's local data directory, mode `0600`.
- The per-window NDJSON archive and the incidents archive, both opt-in, mode `0600`, opened without following symlinks.
- Never raw spans.

The `capture` command is the exception by design: it writes the raw spans it receives, literals included, to an NDJSON file. Treat that file as sensitive.

**What the Hub stores.** The Hub keeps each finding as the engine sent it, in a SQLite database (`/data/hub.db`): template, trace id, grouping attribute values, code location, ack author and reason. It does no masking of its own. Findings are kept 180 days, analysis reports 24 hours and analysis runs 30 days by default.

**Secrets.**
- Environment variables take precedence over the configuration file for the engine's API keys and tokens.
- Endpoint URLs carrying `user:pass@` are rejected at load time, and credentials are redacted from logs.
- The Hub passes a source's credential to the engine subprocess through an environment variable, never on the command line.

## Access control

### Engine

These are never authenticated, whatever the configuration:
- OTLP ingest, gRPC and HTTP.
- `/metrics` and `/health`.
- The read endpoints `/api/findings`, `/api/findings/{trace_id}`, `/api/explain/{id}`, `/api/correlations`, `/api/status`, `/api/config`, `/api/energy` and `/api/export/report`.

The daemon trusts its trace senders. This is the stated threat model: [No authentication](LIMITATIONS.md#no-authentication-tls-available-auth-not-built-in).

Key-protected routes:
- Ack writes (`POST` and `DELETE /api/findings/{signature}/ack`) are open until `[daemon.ack] api_key` or `PERF_SENTINEL_ACK_API_KEY` is set.
- `GET /api/acks` and `GET /api/incidents` then accept that key or `[daemon] read_api_key`.
- `POST /api/incidents` requires a key when it is enabled.

Keys travel in `X-API-Key` or `Authorization: Bearer` and are compared in constant time. The ack author (`by`) is declared by the caller, not authenticated.

CORS is off by default, and a wildcard origin combined with a write key is refused at startup. TLS covers both OTLP listeners when `[daemon.tls]` is set. Client certificates (mTLS) are not supported.

### Hub

- **Findings push:** requires the source's `X-API-Key` (at least 32 characters, bound to its source, compared in constant time). A source without a key cannot push.
- **UI and API:** open by default.
- **Sign-in:** with `hub.auth.enabled`, the UI requires a session obtained through OAuth2 authorization code with PKCE against your identity provider (Keycloak, Entra ID, Google, GitLab and others). The session cookie is `Secure`, `SameSite=Lax`, 8 hours sliding. This is not full OpenID Connect: the identity comes from the userinfo endpoint, the ID token is not validated, and there are no roles.
- **Routes open even with sign-in:** `/api/findings`, `/api/findings/{traceId}`, the import route (which has its own key), `/health` and `/metrics`. Keep them on an internal network.
- **Launcher:** it runs the bundled engine binary as a subprocess with an argument list, without a shell. The binary path and the trace endpoints come from the configuration only. User inputs are validated, and a run stops after 300 s by default.

Details: [Hub authentication](https://github.com/robintra/PerfSentinelHub/blob/main/docs/AUTHENTICATION.md), [Hub limitations](https://github.com/robintra/PerfSentinelHub/blob/main/docs/LIMITATIONS.md).

## Hardening

**Engine code.**
- The core library carries `#![forbid(unsafe_code)]`. The CLI holds four `unsafe` blocks, all `libc` calls (`killpg` to stop a captured process group, `getrusage` for `bench`).
- On musl, the allocator is `mimalloc`, and the TLS stack (`ring`) includes C and assembly.
- Clippy runs in pedantic mode with warnings as errors, and CodeQL analyzes the Rust code.
- The SQL normalizer and the JSON ingest have fuzzing targets in `fuzz/`, run by hand, not in CI.

**Engine limits.**
- Payloads are capped at 16 MiB, compressed and decompressed (`max_payload_size`).
- Concurrency is capped: 32 OTLP HTTP requests, 32 gRPC requests, 256 gRPC streams and 128 socket connections.
- A request times out after one minute.
- A response holds at most 1,000 rows.
- TLS handshakes time out after 10 s, with at most 128 in flight.
- Memory admission control (cgroup v2) is opt-in.
- There is no per-client rate limiting.

**Engine HTML report.** It sets a Content Security Policy and renders every value with `textContent`, and a test fails the build if the template uses an unsafe DOM API.

**Hub code.**
- .NET 10 compiled with NativeAOT, nullable references, warnings as errors.
- Two runtime packages (`Microsoft.Data.Sqlite`, `SQLitePCLRaw`) and parameterized SQL.
- Request bodies are capped at 2 MiB (import, analyses) and 8 KiB (ack), and concurrency gates return `503` with `Retry-After`.
- The Hub sets no security header (CSP, HSTS, `X-Frame-Options`): add them at the ingress.

**Containers and Helm.** Both charts:
- run as non-root with a read-only root filesystem
- drop all capabilities and disallow privilege escalation
- apply the `RuntimeDefault` seccomp profile

The engine chart also disables the service account token. Its NetworkPolicy template is off by default and, once enabled, opens `4317` and `4318` together, so any pod allowed to send traces can also read the query API. The Hub chart ships no NetworkPolicy and no Ingress.

## Supply chain

**Engine.**
- Every GitHub Action is pinned to a commit SHA.
- `Cargo.lock` is committed, and `cargo audit` and `cargo deny` run daily.
- Trivy scans the image before each release and blocks on fixable `HIGH` or `CRITICAL` vulnerabilities.
- Gitleaks scans the full history.
- Release tags are signed, and GitHub shows them as verified.

Release binaries carry:
- a SLSA build provenance attestation, Build Level 2 by GitHub's own documentation
- embedded `cargo-auditable` dependency data
- an attested SPDX SBOM

The Helm chart is signed with Cosign. The container images carry no signature and no attestation. See [Supply chain pinning policy](SUPPLY-CHAIN.md) for the details and [Software supply chain](HELM-DEPLOYMENT.md#software-supply-chain) for the chart.

```bash
gh attestation verify perf-sentinel-linux-amd64 --repo robintra/perf-sentinel
gh attestation verify perf-sentinel-linux-amd64 --repo robintra/perf-sentinel \
  --predicate-type https://spdx.dev/Document/v2.3
cargo audit bin perf-sentinel-linux-amd64
```

**Hub.**
- Every release artefact, the OCI image tarball and the chart included, is signed with Cosign (`sign-blob`) and carries a GitHub build provenance and an SPDX SBOM attestation.
- The release builds the image twice and compares the results byte for byte.
- NuGet restores are locked.
- Actions are pinned by SHA.
- CodeQL, SonarCloud, Trivy, OSV-Scanner, Gitleaks, TruffleHog and OpenSSF Scorecard run in CI.

The image pushed to GHCR is not signed in the registry: verify the release artefacts as described in [Hub releasing](https://github.com/robintra/PerfSentinelHub/blob/main/RELEASING.md).

Neither repository requires signed commits on its main branch today. Release tags are signed on both.

## Reporting a vulnerability

Both projects take reports through GitHub private vulnerability reporting.
- **Engine:** acknowledgment within 72 hours, best effort since the project has a single maintainer. Initial assessment within 7 days. A CVE is requested from Medium severity up. Only the latest minor release receives fixes. See [SECURITY.md](../SECURITY.md).
- **Hub:** acknowledgment within 3 business days, assessment within 7 business days. Only the latest `0.x` release receives fixes. See the [Hub security policy](https://github.com/robintra/PerfSentinelHub/blob/main/SECURITY.md).

## Deployment recommendations

1. Keep OTLP ingestion on a trusted network. The daemon trusts its senders.
2. Enable the engine chart's NetworkPolicy with namespace or pod selectors. To keep the query API from everything that sends traces, put a reverse proxy in front of `4318` that filters `/api/*`.
3. If you use acks, set `PERF_SENTINEL_ACK_API_KEY` from a Secret. Without it, anyone who reaches the port can acknowledge a finding.
4. Encrypt the traffic with `[daemon.tls]`, a service mesh or the ingress.
5. For the Hub, turn on `hub.auth`, terminate TLS and add the security headers at the ingress, and write a NetworkPolicy, since the chart ships none.
6. In a cluster with no egress, set `hub.updateCheck.enabled: false`.
7. Set `http.route` in your instrumentation, and keep personal data out of URL path segments and MySQL double-quoted strings.
8. Treat the files written by `capture` as sensitive. They hold raw spans.
9. If your policy requires signed images, verify the release binary and build or sign the image in your own registry.
