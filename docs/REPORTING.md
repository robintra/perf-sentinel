# Periodic public reporting

`perf-sentinel disclose` produces a single JSON document that aggregates findings collected over a calendar period (typically a quarter) into a form suitable for public transparency. The output is hash-verifiable, schema-versioned, and distinct from the per-batch `Report` JSON consumed by the HTML dashboard.

The subcommand is added in v0.6.x and supersedes earlier ad-hoc disclosure recipes.

## When to use which intent

| intent      | validation       | publishable | typical use                     |
|-------------|------------------|-------------|---------------------------------|
| `internal`  | none             | no          | development drafts, dry runs    |
| `official`  | strict           | yes         | quarterly transparency post     |
| `audited`   | reserved         | not yet     | reserved for a future release   |

`audited` is reserved for a future release. The JSON schema accepts the value for forward compatibility, but the CLI exits with code 2 ("audited intent is reserved for a future release, use 'internal' or 'official' instead") and the daemon refuses to start with `intent = "audited"` configured.

For `official` intent, the validator also rejects reports below 75% runtime-calibration coverage. The denominator is `runtime_windows_count + fallback_windows_count`: each scoring window the daemon archived in the requested period is classified as runtime (per-service energy attribution present) or fallback (proxy I/O share used as a substitute). A coverage below 75% means more than a quarter of the period's windows did not carry per-service attribution, so the proxy share starts dominating the totals and the "official" claim loses meaningful per-service coverage. The empirical rationale for the exact 75% bar (versus 50% or 90%) is documented in [docs/design/08-PERIODIC-DISCLOSURE.md](design/08-PERIODIC-DISCLOSURE.md#the-75-runtime-calibration-threshold).

### Temporal coverage and other warnings (v1.2)

`disclose` reports `aggregate.temporal_coverage`: the fraction of the declared period's calendar days that actually carried measurements (`observed_days / days_in_period`). When it falls below an informational threshold the CLI prints a stderr warning and appends a disclaimer to the report, but it never blocks an `official` report. The reason is that archiving is traffic-gated, a window with no traffic writes nothing, so a low figure can be a legitimately quiet period rather than a measurement gap. Treat it as a lower bound on activity, not a daemon-uptime guarantee. It exists so a reader can distinguish a continuously-measured quarter from one where the daemon ran only a handful of days, the in-binary signal closest to "the operator simply stopped measuring for part of the period". Total non-participation (never running the tool) leaves no report at all and is out of scope for any in-binary check, see [docs/design/08-PERIODIC-DISCLOSURE.md](design/08-PERIODIC-DISCLOSURE.md).

Two further checks tighten consistency. The validator hard-rejects a `days_covered` that does not equal `(to_date - from_date) + 1` (a disclose-produced report always satisfies this, so only a hand-edited file trips it) and a `requests_measured` that exceeds an operator-declared `total_requests_in_period`. For `official` intent, omitting `total_requests_in_period` in the org config emits a warning, since `coverage_percentage` is then absent from the report.

## Granularity

perf-sentinel publishes reports at two granularity levels, controlled by `--confidentiality`. The validator refuses to publish a `confidentiality = public` disclosure that carries G1 entries, and vice versa.

- **G1** (Granularity level 1, "internal detail"). Activated by `--confidentiality internal`. Each `applications[*]` entry carries a full `anti_patterns: [...]` array breaking down every anti-pattern type detected on that service with occurrences, estimated waste energy, and waste carbon. Use for internal optimization decisions, not for public publication: the per-pattern detail exposes internal performance signals an operator may not want broadcast.
- **G2** (Granularity level 2, "public aggregate"). Activated by `--confidentiality public`. Each `applications[*]` entry carries the same service-level totals (energy, carbon, efficiency score) but replaces the array with a single `anti_patterns_detected_count` integer. Suitable for publication on an organization's transparency URL.

## CLI flags

`perf-sentinel disclose` accepts the following flags:

- `--intent <internal|official|audited>` (required). `audited` is reserved for a future release, the CLI refuses it today with exit code 2.
- `--confidentiality <internal|public>` (required). Drives G1 vs G2 granularity, see above.
- `--period-type <calendar-quarter|calendar-month|calendar-year|custom>` (required). Hints the period semantics for downstream consumers. `custom` uses `--from` and `--to` as-is and is the right choice for non-aligned windows (e.g. a 6-week pilot).
- `--from <YYYY-MM-DD>` and `--to <YYYY-MM-DD>` (required, inclusive). UTC calendar dates.
- `--input <PATH>` (required, repeatable). Each path can be a single `.ndjson` file, a directory whose `*.ndjson` files are unioned (sorted by name), or a shell-expanded glob. perf-sentinel itself does not expand globs, so `--input archive/2026Q1/*.ndjson` works in a shell but fails when called via direct `exec` without shell expansion. In CI runners that exec the binary directly, prefer a directory or a single file.
- `--output <PATH>` (required). Where to write `perf-sentinel-report.json`.
- `--org-config <PATH>` (required for `intent = "official"`). The static organisation / methodology / scope TOML described in the previous section.
- `--emit-attestation <PATH>` (optional). When set, also writes the in-toto v1 statement sidecar at this path. Needed for the signing workflow.
- `--strict-attribution` (optional). By default, perf-sentinel buckets spans without a `service.name` attribution into a synthetic `_unattributed` service. This bucket contributes to aggregate totals but is excluded from per-service breakdowns. With `--strict-attribution`, the disclose call refuses to produce a report if any window carries unattributed spans, listing the offending timestamps in the error message. Use for an official disclosure when you want to assert that 100% of measured operations were properly attributed.
- `--tui` (optional, needs the `tui` build feature). Opens a read-only preview instead of writing a report, see below. It relaxes `--intent`, `--confidentiality`, `--period-type`, `--from`, `--to`, and `--output` to optional, since you dial them in the interface. Conflicts with `--emit-attestation`.

## Interactive preview (`--tui`)

`perf-sentinel disclose --tui` opens a read-only preview instead of producing a report. It re-reads the same cold NDJSON archive as the canonical command, so the figures match exactly, but it never hashes or writes anything. Use it to dial in a period and confirm coverage before committing to the reproducible CLI or CI run.

```bash
perf-sentinel disclose --tui \
  --input /var/lib/perf-sentinel/reports.ndjson \
  --org-config /etc/perf-sentinel/org.toml
```

![disclose preview: calendar stepper widens the period, the summary updates, and the official validator reports its verdict](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/disclose/demo.gif)

Only `--input` and `--org-config` stay required. The period, intent, and confidentiality are set live:

- `g` cycles the granularity (month, quarter, year, custom). For the first three, `from` and `to` snap to calendar boundaries. `custom` lets you edit each edge by hand.
- `←` / `→` (or `h` / `l`) step the period one unit at a time. In `custom` they move the active edge by a day, `[` and `]` move it by a month, and `Tab` switches between the `from` and `to` edge.
- `i` toggles the intent (internal or official), `c` toggles the confidentiality (G1 internal or G2 public).
- The summary reports the window count, period coverage against the official threshold, measured and excluded services, totals (requests, carbon, energy, waste ratio), and the official-validator verdict when the intent is official.
- The footer prints the exact `disclose` command for the current settings. Copy it to produce the hashed report.

The preview needs an interactive terminal. Piping its output (no TTY) exits with a clear error rather than rendering.

## Avoidable waste: canonical versus operational (1.1+)

A report carries the total operational energy and carbon of the workload (span-derived, not tunable) plus two tiers of avoidable waste:

- **Canonical** (`aggregate.canonical_waste`). Computed at a fixed N+1 threshold pinned in the binary (`2`), regardless of the operator's `[detection] n_plus_one_threshold`. This is the non-manipulable figure: raising your own threshold cannot shrink it. It is the headline avoidable number, and at the default operator threshold of `5` it is typically larger than what the operator's own dashboard shows.
- **Operational** (`aggregate.operational_waste`). Computed at the operator's configured threshold and recorded with that threshold.

Publishing both keeps the disclosure honest: a reader compares the two and sees how much avoidable waste a loose operator threshold would otherwise hide. For `intent = official`, the validator rejects a report whose `canonical_waste.n_plus_one_threshold` is not the binary's canonical value.

![disclose preview, month view: settings header, aggregated summary, and the equivalent command in the footer](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/disclose/preview.png)

![disclose preview, quarter view: the `g` stepper widens the period to the whole quarter](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/disclose/quarter.png)

![disclose preview, official intent: the validator reports why the report is not yet publishable](https://raw.githubusercontent.com/robintra/perf-sentinel/main/docs/img/disclose/official.png)

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
# Reserved for 0.8.0 (daemon-triggered periodic disclosures), currently
# a no-op. Setting it today logs a warning at startup. Reports are
# produced exclusively via `perf-sentinel disclose --output`.
disclose_output_path = "/var/lib/perf-sentinel/last-disclosure.json"
disclose_period = "calendar-quarter"
```

`intent = "internal"` (or omitting the section) leaves the daemon in monitoring mode without the publishable-disclosure gate.

## Restricting who can publish an official disclosure

Producing an official disclosure is an outward-facing, hard-to-reverse
action: the file lands at a public transparency URL and is signed under
your organisation's identity. The CLI itself has no authorization layer,
anyone who can run the binary with the org-config and the input data can
produce one. So the control belongs in the pipeline that publishes, not
in `perf-sentinel` itself, the same split as for the daemon write paths
(see [`docs/QUERY-API.md`](./QUERY-API.md#restricting-writes-in-production-reverse-proxy)).

In CI, gate the job that runs `disclose --intent official` behind a
**GitHub Environment with required reviewers**. A developer can still
open the PR or trigger the workflow, but the job pauses until a named
reviewer (an architect or a DevOps lead) approves. No approval, no
official report.

Set it up once under `Settings -> Environments -> official-disclosure`:

- **Required reviewers**: the architects or DevOps team that signs off
  on official reports.
- **Deployment branches and tags**: restrict to your protected branch or
  release tags so the job cannot run from an arbitrary branch.

Then target that environment from the workflow:

```yaml
# .github/workflows/perf-sentinel-disclosure.yml
name: official disclosure

on:
  workflow_dispatch:
    inputs:
      from: { description: "period start (YYYY-MM-DD)", required: true }
      to:   { description: "period end (YYYY-MM-DD)",   required: true }

permissions:
  contents: read
  id-token: write   # Sigstore keyless signing via the workflow OIDC token

jobs:
  publish:
    runs-on: ubuntu-latest
    environment: official-disclosure   # required reviewers gate this job
    steps:
      - uses: actions/checkout@b4ffde65f46336ab88eb53be808477a3936bae11 # v4.1.1
      - name: Produce and sign the official disclosure
        run: |
          perf-sentinel disclose \
            --intent official \
            --confidentiality public \
            --period-type calendar-quarter \
            --from "${{ inputs.from }}" --to "${{ inputs.to }}" \
            --input /var/lib/perf-sentinel/reports.ndjson \
            --output perf-sentinel-report.json \
            --org-config /etc/perf-sentinel/org.toml \
            --emit-attestation attestation.intoto.jsonl
          # cosign sign-blob ... then publish, see "Signing your disclosure" below.
```

The `id-token: write` permission is a control in its own right: keyless
signing binds the disclosure to the workflow identity, recorded in
`integrity.signature.signer_identity`. A run from an unapproved path
produces a file whose signer identity does not match the workflow a
verifier expects, see [Identity verification](#identity-verification).

## Signing your disclosure

### Background: Sigstore primer

If you have not used Sigstore before, this short primer is a prerequisite for the commands that follow.

**Why Sigstore.** Sigstore is an open-source toolkit hosted by the Open Source Security Foundation (OpenSSF) and maintained by Google, Red Hat, Chainguard, GitHub and the Linux Foundation. It is the de-facto standard for verifiable artefact signatures in the cloud-native ecosystem (Kubernetes, Helm, npm provenance and PyPI attestations all rely on it). The choice for perf-sentinel disclosures comes from three properties:

1. **Keyless signing**, no long-lived private key to manage or leak on the signer side.
2. **A public, tamper-evident log** (Rekor), so a third party can independently verify that a signature existed at a given point in time.
3. **Free, open-source, self-hostable**, no proprietary lock-in or per-signature billing.

**The three components.**

- **Cosign** is the client CLI you run locally. It opens an OIDC flow in your browser (or consumes a workflow token in CI), signs the file, and ships the signature to Sigstore.
- **Fulcio** is the certificate authority. It takes the OIDC token cosign obtained (proof of identity: email, GitHub workflow URL, ...) and issues a short-lived X.509 certificate (10 minutes) bound to that identity. Fulcio never sees the signer's private key.
- **Rekor** is the public transparency log. It records the signature next to the Fulcio certificate, returns an inclusion proof, and exposes the entry at a stable log index. Past entries cannot be silently rewritten.

**Who signs with which key.** Cosign generates a brand-new ephemeral keypair just before signing. Fulcio issues a 10-minute certificate that binds the *public* half of that keypair to the OIDC identity. Once the signature is uploaded to Rekor the keypair is discarded. What survives is the signature, the certificate, and the Rekor entry, which is exactly what a verifier needs.

**The OIDC identity** is the subject of the Fulcio certificate (and ends up as `integrity.signature.signer_identity` + `signer_issuer` in your disclosure). For an individual signing with a Google account the identity is the email address and the issuer is `https://accounts.google.com`. For a GitHub Actions workflow the identity is the workflow URL and the issuer is `https://token.actions.githubusercontent.com`.

**Known limitation: OIDC issuer migration.** The issuer URL is recorded inside the certificate, therefore in Rekor and in the disclosure. If the organisation later migrates between identity providers (Google Workspace to self-hosted Keycloak, Entra ID to Okta, ...), past signatures remain valid but new signatures will carry a different `signer_issuer`. Verifiers that pin a specific issuer in their verification policy must be updated, otherwise they reject the new signatures as untrusted. Plan the pinning policy with provider migrations in mind.

**Related terms you will see in the workflow.** One-liners only, full definitions in the linked specs.

- **OIDC (OpenID Connect)** is an identity protocol layered on OAuth 2.0. In this workflow it is how cosign proves "this signer is `user@example.org`" to Fulcio. Cosign opens a browser tab, you log in to your IdP (Google, GitHub, ...), the IdP returns a signed token, cosign forwards it to Fulcio. [Spec](https://openid.net/specs/openid-connect-core-1_0.html).
- **in-toto v1 statement** is an open OpenSSF specification for software-supply-chain attestations. A JSON envelope that pairs an artefact hash with a typed *claim* about it. `--emit-attestation` produces such a statement where the artefact is your `report.json` and the claim type is `perf-sentinel-disclosure/v1`. Cosign signs the statement, not the report directly, so verifiers can chain the trust from the report hash through the in-toto statement, the cosign signature on that statement, and finally the Fulcio cert binding the signature to an OIDC identity. [Spec](https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md).
- **Bundle (`bundle.sig`)** is the file cosign writes at sign time. It packs the signature, the Fulcio certificate, and the Rekor inclusion proof into a single JSON. Publishing this bundle next to the report is what enables fully offline verification: a consumer validates the signature against Rekor's public key without having to re-query Rekor live.
- **SLSA provenance** is a separate OpenSSF framework (Supply-chain Levels for Software Artifacts) that describes *how* an artefact was built (which source commit, which builder, which workflow). perf-sentinel release binaries carry SLSA Build L3 provenance produced during the GitHub Actions release workflow. The disclosure's `integrity.binary_attestation` is filled with this provenance when present, and `report_metadata.integrity_level` graduates from `signed` to `signed-with-attestation`. [Spec](https://slsa.dev/spec/v1.0/).

The same primer is mirrored in [docs/SUPPLY-CHAIN.md](SUPPLY-CHAIN.md#background-sigstore-primer), which is the canonical location for the supply-chain stack across binary, Helm chart and disclosure signing.

### Workflow

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

| Field             | Where to read                                                                                                                                                                                                    |
|-------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `format`          | constant `"sigstore-cosign-intoto-v1"` for this schema                                                                                                                                                           |
| `bundle_url`      | URL where you will publish `bundle.sig` at step 4                                                                                                                                                                |
| `signer_identity` | cosign stdout/stderr at step 2, line `Successfully verified SCT...` or `tlog entry... signed by`. Also visible in the cert via `cosign verify-blob --certificate-identity-regexp '.*' ... 2>&1 \| grep identity` |
| `signer_issuer`   | same source as `signer_identity`, the OIDC issuer URL recorded next to it                                                                                                                                        |
| `rekor_url`       | the Rekor instance used (`https://rekor.sigstore.dev` for Sigstore public, or the value from `[reporting.sigstore] rekor_url` for a private instance)                                                            |
| `rekor_log_index` | cosign stdout at step 2, line `tlog entry created with index: X`. Or fetch via `curl <rekor_url>/api/v1/log/entries?logIndex=X` to confirm                                                                       |
| `signed_at`       | timestamp from the Rekor entry, ISO 8601 UTC                                                                                                                                                                     |

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

**Caveat.** The snippet below is indicative, not canonical. It diverges
from the schema in [Editing integrity.signature](#editing-integritysignature)
on four fields:

- `signer_identity`: parses `Successfully signed by ...` from cosign
  stdout, which is the wording emitted by `cosign sign` for container
  images but not always by `cosign sign-blob` (3.0+ omits it). The
  canonical value is the OIDC subject embedded in the signing
  certificate (read it via `cosign verify-blob --certificate-identity-regexp '.*'`,
  or inspect `bundle.sig` directly with
  `jq -r '.verificationMaterial.certificate.rawBytes' bundle.sig | base64 -d | openssl x509 -inform DER -noout -ext subjectAltName`).
- `signer_issuer`: hardcoded to `https://accounts.google.com`. The
  canonical value is the OIDC issuer URL in the signing certificate,
  which the operator must align with their provider (Google, GitHub
  Actions, custom OIDC).
- `rekor_log_index`: parses `tlog entry created with index` from cosign
  stdout, which `cosign sign-blob` 3.0+ no longer emits, so `LOG_INDEX`
  is empty and the snippet falls through to `rekor_log_index: 0`. The
  canonical value is in `bundle.sig` itself at
  `.verificationMaterial.tlogEntries[0].logIndex`, no API call needed.
- `signed_at`: filled with the local clock at jq-run time. The canonical
  value is the `integratedTime` of the Rekor entry, fetched via
  `https://rekor.sigstore.dev/api/v1/log/entries?logIndex=<idx>` and
  formatted as ISO 8601 UTC.

For a publication intended for third-party audit, prefer reading these
four fields from the bundle and from the Rekor entry rather than from
local state. The helper remains useful for an internal dry-run where
the four values are inspected for plausibility, not provenance.

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
config and pass the same URL to `cosign --rekor-url`.
`verify-hash` rejects bundles signed with
`cosign sign-blob --no-tlog-upload`, because such bundles lack a
Rekor inclusion proof. Always sign without that flag for reports
intended for public transparency.

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
    --bundle bundle.sig \
    --expected-identity release@example.fr \
    --expected-issuer https://accounts.google.com

# Remote mode: fetch the report and sidecars by HTTPS convention.
perf-sentinel verify-hash \
    --url https://example.fr/perf-sentinel-report.json \
    --expected-identity release@example.fr \
    --expected-issuer https://accounts.google.com
```

Both examples pass `--expected-identity` and `--expected-issuer` because
that is the safe default: without these flags `verify-hash` refuses to
invoke cosign and returns `Status::Fail` on the signature slot. See
[Identity verification](#identity-verification) below for the three
modes and their semantics. Reuse `--no-identity-check` only for an
internal self-check before publication.

`verify-hash` chains three checks: deterministic content hash
recompute (pure Rust, always run), Sigstore signature
(`cosign verify-blob`), and SLSA binary provenance
(metadata summary plus a `gh attestation verify` command pointing at
the binary in `integrity.binary_verification_url`).

Exit codes:

| Code | Meaning                                                                                                                                          |
|------|--------------------------------------------------------------------------------------------------------------------------------------------------|
| `0`  | TRUSTED (content hash matched AND signature verified ok)                                                                                         |
| `1`  | UNTRUSTED (a check returned a hard failure: hash mismatch, signature invalid, attestation invalid, identity mismatch)                            |
| `2`  | PARTIAL (no hard failure but at least one check could not complete: cosign absent, `gh` CLI absent, signature metadata absent, sidecars missing) |
| `3`  | INPUT_ERROR (report file unreadable, JSON invalid, missing `--report` or `--url`)                                                                |
| `4`  | NETWORK_ERROR (only `--url` mode: HTTP fetch failed, scheme rejected, body over the size cap)                                                    |

A scripted `verify-hash && deploy` gate blocks on any non-zero code
and so still rejects PARTIAL, but a wrapper that distinguishes
PARTIAL (2) from UNTRUSTED (1) can tell a missing tool from a tamper
attempt.

### Sidecar URL convention in `--url` mode

`verify-hash --url <REPORT_URL>` fetches three files from the same
directory, with **fixed filenames**:

```
https://example.fr/<report-filename>            (the report)
https://example.fr/attestation.intoto.jsonl     (in-toto statement sidecar)
https://example.fr/bundle.sig                   (cosign bundle sidecar)
```

The sidecar names are not derived from the report filename: they are
literally `attestation.intoto.jsonl` and `bundle.sig`. An operator
publishing a report must use these filenames at the same URL prefix
for `verify-hash --url` to find them automatically. A future
revision may surface the URLs in `integrity.signature.bundle_url`
so the convention becomes explicit per report, but that is not the
current behaviour.

### Identity verification

`verify-hash` requires the consumer to declare which identity should
have signed the report. Three modes:

- `--expected-identity <ID> --expected-issuer <URL>`: cosign verifies
  that the bundle was issued by exactly this OIDC identity. The
  values come from the auditor's prior knowledge of the publishing
  organization (the report itself declares them in
  `integrity.signature.signer_identity` / `.signer_issuer`, but
  treating those as authoritative is autosigning — any GitHub or
  Google account holder can publish a bundle claiming an identity).
- `--no-identity-check`: cosign verifies the cryptographic integrity
  without checking the identity. Useful for an internal self-check
  before publication, but explicitly logged as PARTIAL because the
  signer is not verified.
- Neither flag passed: `verify-hash` refuses to invoke cosign and
  returns `Status::Fail` on the signature slot. This is the safe
  default and forces an external consumer to declare intent.

### Binary build provenance

`integrity.binary_hash` is the SHA-256 of the perf-sentinel binary
that produced the report. For an official disclosure, the value
should match an official release binary published on the project's
GitHub releases. Operators who build perf-sentinel from source can
still produce official reports, but their `binary_hash` will not
match any published release. In that case
`integrity.binary_attestation` is absent (no SLSA provenance for a
local build) and `verify-hash` reports `[--] Binary attestation:
not provided`. The `integrity_level` is `signed`, not
`signed-with-attestation`. For maximum trust on a publication, use
the released binary matching the tag declared in
`integrity.binary_verification_url`.

## Computing a canonical content hash with `hash-bake` (0.7.2+)

For test fixtures and debugging workflows where you need a report whose `content_hash` already matches what perf-sentinel itself would write, use `hash-bake`:

```bash
perf-sentinel hash-bake --report input.json --output output.json
```

`hash-bake` reads the report at `--report`, computes the canonical `content_hash` (applying the `POST_SIGN_FIELDS` blanching defined for the schema version), writes the hash into `integrity.content_hash`, and saves the result to `--output`. The same path as `--report` is allowed for in-place baking, with an atomic temp+rename that prevents partial corruption.

This command is intended for:

- Generating test fixtures with valid canonical hashes (for example, test suites that exercise `verify-hash` with TRUSTED or PARTIAL outcomes).
- Debugging a report whose hash has drifted from canonical (typically after manual edits to fields outside `POST_SIGN_FIELDS`).

Signed reports (where `integrity.signature` is non-null) are rejected by default. Re-baking does not invalidate the signature, since the canonical form blanches the signature anyway, but the operator should confirm intent with `--allow-signed`.

`hash-bake` does not modify `integrity.signature`, does not modify `integrity.binary_attestation`, and does not modify `report_metadata.integrity_level`. It only writes `integrity.content_hash`.

Exit codes:

| Code | Meaning                                                                                     |
|------|---------------------------------------------------------------------------------------------|
| 0    | Hash baked, file written.                                                                   |
| 1    | Refused: report carries a signature and `--allow-signed` was not passed. No output written. |
| 3    | Input error: report unreadable, JSON invalid, or output cannot be written.                  |

## Common errors

- `Error: audited intent is reserved for a future release, use 'internal' or 'official' instead`: switch `--intent` to `internal` or `official`.
- `no archived reports fell within the requested period`: the archive contains lines but none match the `--from`/`--to` window. Check timestamps, especially around DST and timezone boundaries (the aggregator filters on UTC dates).
- `Error: report validation failed` followed by a bullet list: every line names the offending field. Fix in the org-config TOML or in the source archive.
- `strict_attribution` enabled and a window with no offenders: drop the flag or fix the per-service instrumentation that's hiding the offenders.

## Scope and limitations

The disclosure is a directional estimate with a `2x` multiplicative uncertainty bracket. It is not regulatory-grade and not suitable for CSRD or GHG Protocol Scope 3 reporting. See `docs/METHODOLOGY.md` for the full calculation chain and the calibration sources that can tighten the bracket (Scaphandre RAPL, cloud SPECpower, Electricity Maps).
