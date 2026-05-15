# Periodic public reporting

`perf-sentinel disclose` produces a single JSON document that aggregates findings collected over a calendar period (typically a quarter) into a form suitable for public transparency. The output is hash-verifiable, schema-versioned, and distinct from the per-batch `Report` JSON consumed by the HTML dashboard.

The subcommand is added in v0.6.x and supersedes earlier ad-hoc disclosure recipes.

## When to use which intent

| intent      | validation       | publishable | typical use                     |
|-------------|------------------|-------------|---------------------------------|
| `internal`  | none             | no          | development drafts, dry runs    |
| `official`  | strict           | yes         | quarterly transparency post     |
| `audited`   | reserved         | not yet     | future revision                 |

`audited` is accepted by the JSON schema for forward compatibility but the CLI returns `Error: audited intent is not yet implemented` and exits with code 2.

For `official` intent, the validator also rejects reports below 75% runtime-calibration coverage (see [docs/design/08-PERIODIC-DISCLOSURE.md](design/08-PERIODIC-DISCLOSURE.md#the-75-runtime-calibration-threshold) for the rationale).

## Granularity

- `--confidentiality internal` produces G1 entries per application: the per-anti-pattern breakdown (`anti_patterns: [...]`) is included.
- `--confidentiality public` produces G2 entries: the aggregate per service, plus a single `anti_patterns_detected_count`, but no per-pattern detail.

The validator refuses to publish a `confidentiality = public` disclosure that carries G1 entries.

## Inputs

The aggregator reads NDJSON files that the daemon archives one envelope per scoring window:

```json
{"ts":"2026-01-15T14:30:00Z","report":{ ...full Report... }}
```

Configure the daemon archive via:

```toml
[daemon.archive]
path = "/var/lib/perf-sentinel/reports.ndjson"
max_size_mb = 100
max_files = 12
```

When the active file exceeds `max_size_mb`, perf-sentinel renames it to `reports-<utc-timestamp>.ndjson` and starts a fresh file. Older rotated files beyond `max_files` are pruned by modification time.

Operators that already collect daemon stdout via a sidecar can pass the resulting file (or directory) to `--input` directly, as long as each line is one `{ts, report}` envelope.

## org-config TOML

The static organisation/methodology/scope fields live in a TOML file you check into your infrastructure repository alongside the rest of the perf-sentinel config. A complete example sits in `docs/examples/perf-sentinel-org.toml`. The same file is referenced by `[reporting] org_config_path` when the daemon is asked to validate publishable disclosures at startup.

## Example: internal draft (G1)

```bash
perf-sentinel disclose \
  --intent internal \
  --confidentiality internal \
  --period-type calendar-quarter \
  --from 2026-01-01 --to 2026-03-31 \
  --input /var/lib/perf-sentinel/reports.ndjson \
  --output /tmp/perf-sentinel-report.json \
  --org-config /etc/perf-sentinel/org.toml
```

The output passes only the structural checks (no validator). `integrity.content_hash` is computed and stable but `integrity.binary_hash` is the SHA-256 of the locally running binary, not necessarily a published release.

## Example: official publication (G2)

```bash
perf-sentinel disclose \
  --intent official \
  --confidentiality public \
  --period-type calendar-quarter \
  --from 2026-01-01 --to 2026-03-31 \
  --input /var/lib/perf-sentinel/reports.ndjson \
  --output /var/www/transparency/perf-sentinel-report.json \
  --org-config /etc/perf-sentinel/org.toml
```

The validator runs over the full document. If any required field is missing or out of range, the CLI prints every offending field and exits 2. Fix the org-config (or the underlying data) and re-run.

The recommended publication path is the root of your transparency domain:

```
https://transparency.example.fr/perf-sentinel-report.json
```

The schema URL inside `notes.reference_urls.schema` advertises which schema version a consumer should fetch to validate the file.

## Daemon-driven gating

When the daemon is configured with `[reporting] intent = "official"`, it refuses to start if the org-config TOML is missing or fails the static-field validator. The error message lists every missing or invalid field so an operator fixes them all in one pass.

```toml
[reporting]
intent = "official"
confidentiality_level = "public"
org_config_path = "/etc/perf-sentinel/org.toml"
disclose_output_path = "/var/lib/perf-sentinel/last-disclosure.json"
disclose_period = "calendar-quarter"
```

`intent = "internal"` (or omitting the section) leaves the daemon in monitoring mode without the publishable-disclosure gate.

## Signing your disclosure

`intent = "official"` disclosures should be signed via Sigstore so a
consumer can verify the file was published by your organisation and
has not been modified. The pipeline is opt-in: pass
`--emit-attestation <path>` to `disclose` to get a sidecar in-toto
v1 statement, then sign that statement with `cosign`.

```bash
# 1. Produce the report and the in-toto attestation.
perf-sentinel disclose \
    --intent official \
    --confidentiality public \
    --period-type calendar-quarter \
    --from 2026-01-01 --to 2026-03-31 \
    --input archive/2026Q1/*.ndjson \
    --output report.json \
    --emit-attestation attestation.intoto.jsonl \
    --org-config org.toml

# 2. Sign the attestation with cosign against Sigstore public. The
#    file produced at step 1 is already a complete in-toto v1
#    Statement, so we sign it directly with `cosign sign-blob`. The
#    OIDC issuer (browser flow or GitHub Actions token) records the
#    signer identity. The bundle includes the Rekor inclusion proof.
#    Do NOT use `cosign attest-blob --predicate attestation.intoto.jsonl`:
#    that command treats its input as a raw predicate and wraps it in
#    a fresh Statement, producing a double-wrapped permanent entry in
#    the Rekor public log.
cosign sign-blob \
    --bundle bundle.sig \
    --new-bundle-format \
    attestation.intoto.jsonl

# 3. Patch integrity.signature into report.json so verifiers can
#    locate the bundle and the Rekor entry (see "Editing
#    integrity.signature" below for the schema and a jq helper).
#    Then bump report_metadata.integrity_level from "hash-only" to
#    "signed" (or "signed-with-attestation" if the producing binary
#    carries SLSA provenance). A future `perf-sentinel sign`
#    subcommand will automate this step.

# 4. Publish report.json, attestation.intoto.jsonl, bundle.sig at
#    your transparency URL.
```

### Editing integrity.signature

After step 2 succeeds, `report.json` still has `integrity.signature =
null`. A consumer running `verify-hash` would see "Signature: not
provided" and treat the report as PARTIAL. Step 3 fills the locator
fields so the consumer can find the bundle and verify it.

The seven fields and where each value comes from:

| Field | Where to read |
|-------|---------------|
| `format` | constant `"sigstore-cosign-intoto-v1"` for this schema |
| `bundle_url` | URL where you will publish `bundle.sig` at step 4 |
| `signer_identity` | cosign stdout/stderr at step 2, line `Successfully verified SCT...` or `tlog entry... signed by`. Also visible in the cert via `cosign verify-blob --certificate-identity-regexp '.*' ... 2>&1 \| grep identity` |
| `signer_issuer` | same source as `signer_identity`, the OIDC issuer URL recorded next to it |
| `rekor_url` | the Rekor instance used (`https://rekor.sigstore.dev` for Sigstore public, or the value from `[reporting.sigstore] rekor_url` for a private instance) |
| `rekor_log_index` | cosign stdout at step 2, line `tlog entry created with index: X`. Or fetch via `curl <rekor_url>/api/v1/log/entries?logIndex=X` to confirm |
| `signed_at` | timestamp from the Rekor entry, ISO 8601 UTC |

Example before / after on a fresh disclosure:

```json
// Before step 2 (state immediately after disclose --emit-attestation)
"integrity": {
  "content_hash": "sha256:abc123...",
  "binary_hash": "sha256:def456...",
  "binary_verification_url": "https://github.com/robintra/perf-sentinel/releases/tag/v0.7.0",
  "trace_integrity_chain": null,
  "signature": null,
  "binary_attestation": null
}
```

```json
// After step 3 (after cosign sign-blob succeeds and the operator pastes locators)
"integrity": {
  "content_hash": "sha256:abc123...",
  "binary_hash": "sha256:def456...",
  "binary_verification_url": "https://github.com/robintra/perf-sentinel/releases/tag/v0.7.0",
  "trace_integrity_chain": null,
  "signature": {
    "format": "sigstore-cosign-intoto-v1",
    "bundle_url": "https://transparency.example.fr/bundle.sig",
    "signer_identity": "robin.trassard@example.fr",
    "signer_issuer": "https://accounts.google.com",
    "rekor_url": "https://rekor.sigstore.dev",
    "rekor_log_index": 123456789,
    "signed_at": "2026-05-15T09:00:00Z"
  },
  "binary_attestation": null
}
```

And in `report_metadata`:

```diff
-  "integrity_level": "hash-only"
+  "integrity_level": "signed"
```

(Use `"signed-with-attestation"` instead of `"signed"` when the
producing binary also carries SLSA provenance.)

### Content hash stays valid

The `content_hash` does **not** need to be recomputed after step 3.
The canonical form used by `compute_content_hash` blanks four
fields before hashing: `integrity.content_hash`,
`integrity.signature`, `integrity.binary_attestation`, and
`report_metadata.integrity_level`. The list lives in
`POST_SIGN_FIELDS` (`crates/sentinel-core/src/report/periodic/hasher.rs`)
and the invariance is enforced by the test
`hash_is_invariant_under_post_sign_locator_addition`. So a consumer
re-running the hash on the post-step-3 report gets the same value
as the operator did at step 1.

**Do not recompute** `content_hash` after editing. Doing so
produces a fresh hash, breaks the canonical form, and a verifier
will see a hash mismatch.

### jq helper

The pattern is repetitive and easy to script. Until
`perf-sentinel sign` lands (planned for 0.7.x), this jq workflow
captures the fields from cosign output and patches the report in
one shot:

```bash
# Sign and capture cosign output for parsing
cosign sign-blob \
    --bundle bundle.sig \
    --new-bundle-format \
    attestation.intoto.jsonl 2>&1 | tee cosign.log

# Extract tlog index from cosign output. Format:
# "tlog entry created with index: 123456789"
LOG_INDEX=$(grep "tlog entry created with index" cosign.log \
            | awk '{print $NF}')

# Extract signer identity from the cosign log. Format depends on
# issuer: for Google OIDC it's an email, for GitHub Actions it is
# the workflow URL.
SIGNER=$(grep "Successfully signed" cosign.log \
         | sed 's/.*by //' | tr -d '"')

# Pick the issuer that matches your OIDC provider.
ISSUER="https://accounts.google.com"  # or token.actions.githubusercontent.com

# Patch report.json with the seven locator fields and bump
# integrity_level. Adjust bundle_url to your transparency host.
jq --arg url "https://transparency.example.fr/bundle.sig" \
   --arg sig "$SIGNER" \
   --arg issuer "$ISSUER" \
   --arg idx "$LOG_INDEX" \
   --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
   '.integrity.signature = {
     format: "sigstore-cosign-intoto-v1",
     bundle_url: $url,
     signer_identity: $sig,
     signer_issuer: $issuer,
     rekor_url: "https://rekor.sigstore.dev",
     rekor_log_index: ($idx | tonumber),
     signed_at: $ts
   } | .report_metadata.integrity_level = "signed"' \
   report.json > report-signed.json && mv report-signed.json report.json
```

This is an interim workaround. `perf-sentinel sign` will replace
the bash + jq combo with a single subcommand once it ships.

Operators who run a private Rekor instance set
`[reporting.sigstore] rekor_url = "..."` in their perf-sentinel
config and pass the same URL to `cosign --rekor-url`. Reports
produced without `--no-tlog-upload` only: `verify-hash` rejects
bundles without a Rekor inclusion proof.

`verify-hash` itself reads `integrity.signature.rekor_url` from
the report being verified, so a consumer fetching a publicly
hosted disclosure does not need any local configuration: the
URL travels with the report. If you want to force a different
Rekor at verification time (e.g. cross-check a public-Rekor
claim against a private archive), invoke cosign directly with
its own `--rekor-url` flag rather than going through
`verify-hash`. The report stays the single source of truth for
which transparency log signed it.

See `docs/design/10-SIGSTORE-ATTESTATION.md` for the full
methodology, failure modes, and privacy considerations on Rekor
public.

## Verifying a published disclosure

A third party verifies a published file with one command:

```bash
# Local mode: all three files already downloaded.
perf-sentinel verify-hash \
    --report report.json \
    --attestation attestation.intoto.jsonl \
    --bundle bundle.sig

# Remote mode: fetch the report and sidecars by HTTPS convention.
perf-sentinel verify-hash --url https://example.fr/perf-sentinel-report.json
```

`verify-hash` chains three checks: deterministic content hash
recompute (pure Rust, always run), Sigstore signature
(`cosign verify-blob`), and SLSA binary provenance
(metadata summary plus an `slsa-verifier` command pointing at the
binary in `integrity.binary_verification_url`).

Exit codes:

| Code | Meaning |
|------|---------|
| `0` | TRUSTED (content hash matched AND signature verified ok) |
| `1` | UNTRUSTED (a check returned a hard failure: hash mismatch, signature invalid, attestation invalid, identity mismatch) |
| `2` | PARTIAL (no hard failure but at least one check could not complete: cosign absent, slsa-verifier absent, signature metadata absent, sidecars missing) |
| `3` | INPUT_ERROR (report file unreadable, JSON invalid, missing `--report` or `--url`) |
| `4` | NETWORK_ERROR (only `--url` mode: HTTP fetch failed, scheme rejected, body over the size cap) |

A scripted `verify-hash && deploy` gate blocks on any non-zero code
and so still rejects PARTIAL, but a wrapper that distinguishes
PARTIAL (2) from UNTRUSTED (1) can tell a missing tool from a tamper
attempt.

## Common errors

- `Error: audited intent is not yet implemented`: switch `--intent` to `internal` or `official`.
- `no archived reports fell within the requested period`: the archive contains lines but none match the `--from`/`--to` window. Check timestamps, especially around DST and timezone boundaries (the aggregator filters on UTC dates).
- `Error: report validation failed` followed by a bullet list: every line names the offending field. Fix in the org-config TOML or in the source archive.
- `strict_attribution` enabled and a window with no offenders: drop the flag or fix the per-service instrumentation that's hiding the offenders.

## Scope and limitations

The disclosure is a directional estimate with a `2x` multiplicative uncertainty bracket. It is not regulatory-grade and not suitable for CSRD or GHG Protocol Scope 3 reporting. See `docs/METHODOLOGY.md` for the full calculation chain and the calibration sources that can tighten the bracket (Scaphandre RAPL, cloud SPECpower, Electricity Maps).
