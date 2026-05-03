# Acknowledgments

A way to tell perf-sentinel "yes, this finding is real, and we have decided not to fix it (yet)". Acknowledged findings are filtered from the CLI output and excluded from the quality gate. The decisions live in `.perf-sentinel-acknowledgments.toml` at the root of the repo, so every change goes through normal PR review and `git log` is the audit trail.

This document covers the file format, the workflow, the CLI flags, and the FAQ.

## When to use it

- The team has decided that a finding is intentional (cache invalidation pattern, batched-on-purpose work, throwaway script with O(N) calls).
- A long-lived workaround that is tracked elsewhere (Jira, ADR) and that you do not want flagged on every CI run until you can fix the root cause.
- A finding that flapped under a noisy traffic shape and that the team agreed to revisit when the upstream issue is resolved.

If you are on the fence, prefer **NOT** acking it. Each ack hides a real signal. The threshold should be "we discussed it, we decided".

## The file

Path: `./.perf-sentinel-acknowledgments.toml` at the root of the repository where you run `perf-sentinel`. Override with `--acknowledgments <path>`.

```toml
# .perf-sentinel-acknowledgments.toml
#
# This file documents perf-sentinel findings that have been acknowledged
# by the team as known and intentional. Acknowledged findings are
# filtered from the CLI output (analyze, report, inspect, diff) and do
# not count toward the quality gate.
#
# Each entry is matched against the finding's signature, computed as:
#   <finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>
#
# To get a finding's signature:
#   perf-sentinel analyze --input traces.json --format json | jq '.findings[].signature'

[[acknowledged]]
signature = "redundant_sql:order-service:POST__api_orders:cafebabecafebabe"
acknowledged_by = "alice@example.com"
acknowledged_at = "2026-05-02"
reason = "Cache invalidation pattern, intentional. See ADR-0042."
expires_at = "2026-12-31"  # Optional, omit for permanent.

[[acknowledged]]
signature = "slow_sql:report-service:GET__api_reports:deadbeefdeadbeef"
acknowledged_by = "bob@example.com"
acknowledged_at = "2026-04-15"
reason = "Long-running aggregation, accepted by product."
# No expires_at: this ack is permanent.
```

### Field reference

| Field             | Required | Notes                                                                     |
|-------------------|----------|---------------------------------------------------------------------------|
| `signature`       | yes      | Canonical finding signature (see below).                                  |
| `acknowledged_by` | yes      | Email or identifier. Free text.                                           |
| `acknowledged_at` | yes      | ISO 8601 date `YYYY-MM-DD`. Free text, not validated.                     |
| `reason`          | yes      | Free text. Keep it short and link to ADR / Jira / Slack thread.           |
| `expires_at`      | no       | ISO 8601 date `YYYY-MM-DD`. Validated at load time. Omit for a permanent ack. |

A missing required field fails the run with a clear error so a typo does not silently widen the acked set.

## Signature format

```
<finding_type>:<service>:<sanitized_endpoint>:<sha256-prefix-of-template>
```

- `finding_type` is the snake-case enum: `n_plus_one_sql`, `redundant_sql`, `slow_http`, `chatty_service`, etc.
- `service` is the OpenTelemetry service name as captured in the trace (e.g. `order-service`).
- `sanitized_endpoint` is `source_endpoint` with `/` and spaces replaced by `_` so the result splits cleanly on `:`.
- `sha256-prefix-of-template` is the first 16 hex chars (8 bytes) of `sha256(pattern.template)`. ~64 bits of collision resistance. Since the `(finding_type, service, sanitized_endpoint)` triple is already part of the signature, the hash only needs to disambiguate templates within the same triple, which is an extremely small population in practice. The 16-char prefix is defense in depth against accidental ack masking after a SQL refactor or a service rename.

Three findings produce three different signatures. Two findings produced by the same template on the same `(service, source_endpoint)` collapse to the same signature, which is the right semantics: ack once, suppress every recurrence.

## Workflow

1. Run perf-sentinel and identify the finding you want to ack.
2. Capture its signature:
   ```bash
   perf-sentinel analyze --input traces.json --format json \
     | jq -r '.findings[] | select(.service == "order-service") | .signature'
   ```
3. Open a PR that adds a `[[acknowledged]]` block to `.perf-sentinel-acknowledgments.toml`. Discuss the `reason` in PR review.
4. Merge. The next CI run reads the updated file and the finding stops appearing.

`git log .perf-sentinel-acknowledgments.toml` gives you the full audit history.

## CLI flags

The flags work uniformly on `analyze`, `report`, `inspect`, `diff`.

| Flag                          | Effect                                                                                                                            |
|-------------------------------|-----------------------------------------------------------------------------------------------------------------------------------|
| (default, no flag)            | Loads `./.perf-sentinel-acknowledgments.toml` if present, applies it. No file = no-op, current behavior preserved.                |
| `--acknowledgments <path>`    | Override the default path. Useful in monorepos with one ack file per service folder.                                              |
| `--no-acknowledgments`        | Disable filtering completely. Use for full audit views ("show me everything I have acked too").                                   |
| `--show-acknowledged`         | Apply filtering, but include the acked findings in the output with their ack metadata. Useful for periodic ack review.            |

## Quality gate behavior

Acknowledged findings are excluded from the quality gate computation. In other words: a finding that would have failed `n_plus_one_sql_critical_max = 0` becomes a PASS once acked.

This is the entire point of "won't fix / accepted" semantics. If you do not want this behavior, do not ack the finding, lower the threshold, or use `--no-acknowledgments` in CI.

## What about the `io_waste_ratio_max` rule?

The `io_waste_ratio_max` rule reads from `green_summary.io_waste_ratio`, which is computed from raw spans, not from the findings list. Acknowledging an N+1 finding does **not** lower the waste ratio, because the underlying I/O operations are still real and still happen.

Decision: this is the right behavior. An ack means "the team accepted this finding, do not flag it". It does not mean "pretend the I/O work is not happening". The carbon and waste numbers are honest accounting, the alert routing is what the ack controls.

## FAQ

**Q: How do I migrate a temporary ack to permanent?**
Remove the `expires_at` line and re-commit. PR review captures the decision.

**Q: How do I debug an ack that does not match?**
Run `perf-sentinel analyze --no-acknowledgments --format json | jq '.findings[].signature'`, compare the value to what is in the TOML file. Common causes: the template normalized differently after a code change, the service name changed, the endpoint route was renamed.

**Q: Can I ack a finding by service or by type, with wildcards?**
No, signature-only matching is intentional in 0.5.17. Wildcards make it too easy to silence categories of finding by accident. If you want to ack 10 N+1 findings on a service, open 10 PRs (or one PR with 10 entries), one signature each.

**Q: What if I commit an ack that turns out to be wrong?**
Revert the commit. The next CI run will re-surface the finding.

**Q: Is there an `acknowledgments` API on the daemon?**
Not in 0.5.17. The daemon path is on the roadmap (deferred to a later release pending architecture review), the CI/batch path covers the bulk of the use cases.

**Q: Does `inspect` (TUI) honor acknowledgments?**
Yes, the same flags apply. The TUI does not yet have a dedicated panel to show suppressed findings, but the status footer surfaces the count.

**Q: Does the HTML dashboard surface ack metadata?**
With `--show-acknowledged`, the embedded JSON payload includes the `acknowledged_findings` array (visible in DevTools or with `jq` against the embedded data). The visual UI does not yet render a dedicated ack section, that is on the dashboard roadmap.

## SARIF integration

Starting in 0.5.18, the SARIF emitter exposes the finding signature in two places, so CI tools that consume SARIF (GitHub Code Scanning, GitLab SAST, Sonar) can match findings against `.perf-sentinel-acknowledgments.toml` without parsing the JSON output separately.

- `runs[].results[].properties.signature` carries the canonical signature string, consistent with the other ack fields already in `properties` (`acknowledged`, `acknowledgmentReason`, ...).
- `runs[].results[].fingerprints["perfsentinel/v1"]` exposes the same value through the SARIF v2.1.0 native `fingerprints` mechanism (section 3.27.17), used by GitHub Code Scanning and GitLab SAST for deduplication across runs.

Both fields hold the same value, pick whichever one matches your tool's ingestion model. Findings deserialized from baselines produced before 0.5.17 have an empty signature and the SARIF emitter omits both fields for them (graceful degradation).

See [`SARIF.md`](SARIF.md) for the full per-result field reference.

## Cross-references

- [`README.md`](../README.md) section "Acknowledging known findings" for the quick pitch.
- [`CONFIGURATION.md`](CONFIGURATION.md) for how `.perf-sentinel.toml` and `.perf-sentinel-acknowledgments.toml` interact.
- [`RUNBOOK.md`](RUNBOOK.md) section "Investigating an unexpected ack" for the on-call recipe.
