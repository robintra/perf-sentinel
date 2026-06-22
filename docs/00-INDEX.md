# Documentation index

This directory contains the user-facing documentation for perf-sentinel. For deep design rationale aimed at contributors, see the [`design/`](design/00-INDEX.md) sub-directory.

A French mirror of every document lives under [`FR/`](FR/00-INDEX-FR.md).

## Getting started

| Document                                 | Description                                                                       |
|------------------------------------------|-----------------------------------------------------------------------------------|
| [ARCHITECTURE.md](ARCHITECTURE.md)       | Pipeline overview, module responsibilities, key types                             |
| [INSTRUMENTATION.md](INSTRUMENTATION.md) | Per-language OTLP setup: Java, Quarkus, .NET, Go, Python, Node.js, Rust           |
| [CI.md](CI.md)                           | CI mode, GitHub Actions / GitLab CI / Jenkins recipes, PR regression detection    |

## Deployment

| Document                                 | Description                                                                       |
|------------------------------------------|-----------------------------------------------------------------------------------|
| [INTEGRATION.md](INTEGRATION.md)         | Four deployment topologies (batch, sidecar, gateway, standalone) and quick starts |
| [HELM-DEPLOYMENT.md](HELM-DEPLOYMENT.md) | Kubernetes deployment via the Helm chart, values reference, TLS, RBAC             |

## Reference

| Document                             | Description                                                                                                       |
|--------------------------------------|-------------------------------------------------------------------------------------------------------------------|
| [CONFIGURATION.md](CONFIGURATION.md) | Full `.perf-sentinel.toml` reference (thresholds, detection, GreenOps, daemon)                                    |
| [CLI.md](CLI.md)                     | Subcommand reference (`analyze`, `watch`, `report`, `diff`, `query`, `ack`, `inspect`, `disclose`, `verify-hash`) |
| [METRICS.md](METRICS.md)             | Prometheus metrics exposed by the daemon on `/metrics`                                                            |
| [QUERY-API.md](QUERY-API.md)         | Daemon HTTP API (`/api/findings`, `/api/correlations`, `/api/explain/{trace}`, `/api/status`)                     |
| [SARIF.md](SARIF.md)                 | SARIF v2.1.0 output format for IDE and GitHub Code Scanning integration                                           |
| [SCHEMA.md](SCHEMA.md)               | JSON Schema for the periodic disclosure report (`perf-sentinel-report v1.0`)                                      |

## Features

| Document                                 | Description                                                                                                                      |
|------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------|
| [HTML-REPORT.md](HTML-REPORT.md)         | Self-contained HTML dashboard, live mode via `--daemon-url`, ack/revoke from the browser                                         |
| [INSPECT.md](INSPECT.md)                 | Interactive TUI: Analyze/Inspect/Explain drill-down (`analyze --tui`, `inspect`, `explain --tui`), arrow or vim keys, ack/revoke |
| [ACKNOWLEDGMENTS.md](ACKNOWLEDGMENTS.md) | `.perf-sentinel-acknowledgments.toml` format, SHA-256 signatures, filtering rules                                                |
| [ACK-WORKFLOW.md](ACK-WORKFLOW.md)       | End-to-end acknowledgment workflow across daemon API, CLI, TUI, and HTML report                                                  |
| [REPORTING.md](REPORTING.md)             | Periodic public disclosure via `perf-sentinel disclose`, schema versioning, hash verification                                    |

## Operations

| Document                         | Description                                                                         |
|----------------------------------|-------------------------------------------------------------------------------------|
| [RUNBOOK.md](RUNBOOK.md)         | Incident runbook: symptom-driven troubleshooting for production deployments         |
| [METHODOLOGY.md](METHODOLOGY.md) | Calculation chain from traces to `efficiency_score`, `energy_kwh`, `carbon_kgco2eq` |
| [LIMITATIONS.md](LIMITATIONS.md) | Known trade-offs, upstream constraints, detection boundaries                        |

## Supply chain and release

| Document                                     | Description                                                                         |
|----------------------------------------------|-------------------------------------------------------------------------------------|
| [SUPPLY-CHAIN.md](SUPPLY-CHAIN.md)           | Build input pinning, Sigstore signing, SLSA provenance, `verify-hash` chain         |
| [RELEASE-PROCEDURE.md](RELEASE-PROCEDURE.md) | End-to-end release procedure from 0.7.0 onwards, simulation-lab gate, Helm lockstep |

## Sub-directories

| Directory                        | Contents                                                    |
|----------------------------------|-------------------------------------------------------------|
| [`design/`](design/00-INDEX.md)  | Deep design documentation (10 chapters), contributor-facing |
| [`FR/`](FR/00-INDEX-FR.md)       | French mirror of all documents                              |
| [`ci-templates/`](ci-templates/) | CI snippet files referenced by `CI.md`                      |
| [`schemas/`](schemas/)           | JSON Schema and examples for the disclosure report          |
| [`diagrams/`](diagrams/)         | Architecture diagrams                                       |
| [`examples/`](examples/)         | Example configurations and outputs                          |
| [`img/`](img/)                   | Generated terminal GIFs and PNGs (VHS tapes)                |
