# Security policy

Thank you for taking the time to improve perf-sentinel's security. This document explains how to report a vulnerability and what to expect in return.

## Supported versions

perf-sentinel follows semantic versioning. Security fixes are backported as follows:

| Version | Supported |
|---------|-----------|
| 0.5.x   | ✅         |
| < 0.5   | ❌         |

Only the latest minor release receives security fixes. Users on older versions are encouraged to upgrade.

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues, discussions, or pull requests.**

Instead, report them privately through GitHub's built-in private vulnerability reporting:

1. Navigate to the [Security tab](https://github.com/robintra/perf-sentinel/security) of this repository.
2. Click **Report a vulnerability**.
3. Fill in the advisory form with as much detail as possible.

Alternatively, you can open a private security advisory directly at:
https://github.com/robintra/perf-sentinel/security/advisories/new

### What to include in your report

To help us triage quickly, please include:

- A clear description of the vulnerability and its potential impact.
- Steps to reproduce, ideally with a minimal proof-of-concept.
- The affected version(s) of perf-sentinel.
- Any relevant configuration (e.g., daemon listen address, enabled scrapers, TLS config).
- Your assessment of severity, if you have one.

### What to expect

- **Acknowledgment**: within 72 hours (best-effort, this is a solo-maintained project).
- **Initial assessment**: within 7 days, including a severity rating and a tentative fix timeline.
- **Fix and disclosure**: coordinated through a GitHub Security Advisory. A CVE will be requested for vulnerabilities rated Medium or higher.
- **Credit**: if you wish, you will be credited in the advisory and in the release notes of the fix.

## Scope

The following components are in scope for security reports:

- The `perf-sentinel` binary and its subcommands (`analyze`, `watch`, `query`, `diff`, `explain`, `inspect`, `pg-stat`, `bench`, `tempo`, `calibrate`, `demo`).
- The `perf-sentinel-core` library crate.
- Network listeners: OTLP gRPC (port 4317), OTLP HTTP, `/metrics`, `/health`, and the query API endpoints (`/api/*`).
- Opt-in outbound scrapers: Scaphandre, cloud energy (AWS/GCP/Azure), Electricity Maps, pg_stat (Prometheus), Tempo.
- Configuration file parsing (`.perf-sentinel.toml`).
- SARIF, JSON, OpenMetrics output.
- Docker images published to Docker Hub (`robintrassard/perf-sentinel`) and GHCR (`ghcr.io/robintra/perf-sentinel`).

### Out of scope

- Vulnerabilities in third-party dependencies that do not affect perf-sentinel's behavior. Those should be reported upstream. We track advisory status via `cargo audit` (see `.github/workflows/security-audit.yml` and `audit.toml` for documented non-applicable advisories).
- Denial-of-service reports that require the attacker to already have privileged access to the daemon's configuration or to the trusted OTLP input channel (perf-sentinel's threat model assumes trusted trace producers).
- Security of the user's own OTel pipeline, Prometheus, Grafana, or any downstream system.
- Issues specific to running perf-sentinel with `listen_address = "0.0.0.0"` without a reverse proxy, firewall, or network policy. The default is `127.0.0.1` for a reason; exposing the daemon directly to untrusted networks is explicitly discouraged in `docs/LIMITATIONS.md`.

## Automated security checks

The following scans run in CI and block the pipeline on severity `HIGH` or `CRITICAL`:

- **cargo audit** (Rust dependency vulnerabilities): scheduled daily and on every `Cargo.toml` / `Cargo.lock` change. Documented non-applicable advisories live in `audit.toml`. See `.github/workflows/security-audit.yml`.
- **Clippy with pedantic lints** plus SARIF upload to GitHub Code Scanning: every CI run. Catches logic and API-design issues.
- **Trivy** (container image vulnerabilities): runs on every release tag before the image is pushed to GHCR or Docker Hub. `ignore-unfixed` is enabled so unpatched upstream CVEs do not block the release. SARIF output is uploaded to GitHub Code Scanning.
- **Gitleaks** (secret scan): runs on every push and pull request, scanning the full git history with the bundled default ruleset (AWS keys, GitHub tokens, JWT, private keys, etc.).
- **SonarCloud** (code quality and security hotspots): runs when a `SONAR_TOKEN` secret is available, skipped on Dependabot PRs that do not receive repo secrets.

## Security-relevant design choices

For context, the following choices are deliberate and documented:

- **Default bind to `127.0.0.1`**: the daemon never listens on all interfaces by default.
- **Payload size limits**: JSON/OTLP payloads are bounded (`max_payload_size`, default 1 MB).
- **No default outbound network**: scrapers are opt-in and only connect to explicitly configured endpoints.
- **Credentials rejected at config load**: endpoint URLs containing `user:pass@` are rejected with a clear error; secrets must come from environment variables.
- **Log redaction**: credentials are redacted in all scraper logs via `redact_endpoint`.
- **TLS for OTLP listeners**: opt-in via `[daemon.tls]`. The recommended production pattern remains a reverse proxy (envoy, nginx) for broader TLS feature coverage.
- **SARIF path sanitization**: filepaths from SARIF output are validated against path traversal, control characters, bidi overrides, and overlong UTF-8 encodings.
- **HTML dashboard: `textContent`-only rendering**: every user-controlled value (SQL templates, service names, HTTP URLs, trace IDs, code locations, `SuggestedFix` text) is embedded in a `<script id="report-data" type="application/json">` block and rendered exclusively via `Element.textContent` and `document.createElement()`. The template never calls `innerHTML`, `insertAdjacentHTML`, `outerHTML`, `document.write`, `eval`, `new Function`, `DOMParser`, `createContextualFragment`, or `setAttribute` with an `on*` attribute name. A unit test (`no_forbidden_apis_in_template` in `crates/sentinel-core/src/report/html.rs`) greps the template on every build and fails CI if any of those strings appear.
- **HTML dashboard: script-tag break-out defense**: the Rust injector escapes the substring `</` to `<\/` in the serialized JSON payload so a user-controlled string cannot close the `<script>` block early. `\/` is a permitted JSON string escape, `JSON.parse` round-trips the original value unchanged.
- **HTML dashboard: prototype-pollution hardening**: every lookup map keyed by user-controlled identifiers (`trace_id`, `service`, `span_id`, `parent_span_id`, `normalized_template`) is created with `Object.create(null)` so a hostile identifier like `"__proto__"` cannot reparent the object chain.
- **HTML dashboard: CSV formula-injection guard**: every cell in exported CSVs is prefixed with a single apostrophe when its first character is `=`, `+`, `-`, `@`, or a tab, per OWASP CSV injection guidance. Excel, LibreOffice and Google Sheets display the original text without evaluating it as a formula.
- **HTML dashboard: deep-link hash allowlist**: keys accepted from the URL fragment are restricted to `search`, `ranking`, `severity`, `service`. A hostile hash like `#x&__proto__=y` cannot pollute internal state.
- **HTML dashboard: self-contained output**: the generated file has no `<link rel="stylesheet">`, no `<script src="...">`, no web fonts, no images, no CDN. It loads offline from a `file://` URL with zero network requests, which makes it trivially auditable and removes any supply-chain vector through bundled resources.

See `docs/LIMITATIONS.md` (EN) and `docs/FR/LIMITATIONS-FR.md` (FR) for the full threat model and operational caveats.
