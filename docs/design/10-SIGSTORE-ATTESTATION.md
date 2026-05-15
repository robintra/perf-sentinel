# Sigstore signature and SLSA attestation

This document describes the cryptographic primitives layered on top of
the `perf-sentinel-report/v1.0` schema starting with v0.7.0. The goal
is to let a consumer verify a published periodic disclosure end to
end without trusting perf-sentinel or the publishing organisation
beyond what is anchored in Sigstore public infrastructure.

## Why two layers

perf-sentinel reports rely on two complementary signatures:

- **Sigstore signature** on the report itself, anchored in the Rekor
  transparency log. Proves the report was signed by an identity
  authorised by the publishing organisation and has not been modified
  since.
- **SLSA build provenance** on the perf-sentinel binary, produced by
  the project's GitHub Actions release workflow. Proves the binary
  that computed the report was built from the official source tree by
  a recognised builder, not by a custom or tampered build.

A consumer who verifies both gains a complete chain of trust:

```
source code -> SLSA attestation -> binary -> report -> Sigstore signature
```

The two layers are independent: an operator can sign a report
produced by a non-official binary (the signature still proves
authorship and integrity, the binary attestation will simply be
absent from the report). Or an official binary can produce a
report that is never signed (hash-only). The schema makes both
states explicit through `integrity.integrity_level`:

| level                      | content_hash | signature | binary_attestation |
|----------------------------|--------------|-----------|--------------------|
| `none`                     | absent       | absent    | absent             |
| `hash-only`                | present      | absent    | absent             |
| `signed`                   | present      | present   | absent             |
| `signed-with-attestation`  | present      | present   | present            |
| `audited` (reserved)       | n/a          | n/a       | n/a                |

## The attestation flow

For an `intent = "official"` disclosure, the operator workflow is:

1. **Scoring**: the daemon writes per-window archives to NDJSON over
   the period (no signature involvement).
2. **Disclose**: `perf-sentinel disclose --intent official ...
   --output report.json --emit-attestation attestation.intoto.jsonl`
   produces two files. The report's `integrity.content_hash` is
   filled with the canonical SHA-256. The attestation is an in-toto
   v1 statement whose `subject.digest.sha256` pins the SHA-256 of the
   report file on disk (not the canonical hash, which blanks one
   field).
3. **Sign**: the operator runs `cosign sign-blob --bundle bundle.sig
   --new-bundle-format attestation.intoto.jsonl` against Sigstore
   public. The signature is uploaded to Rekor automatically (the
   project rejects bundles without a Rekor inclusion proof at
   verification time). The statement is signed as-is, no extra
   wrapping. Using `cosign attest-blob --predicate` here would wrap
   the already-formed statement in a fresh predicate-of-statement,
   producing a permanent malformed entry in the Rekor public log.
4. **Update the report's signature locator**: the operator edits
   `report.json` to add `integrity.signature` with the metadata
   that lets verifiers locate the bundle and Rekor entry, then
   bumps `integrity_level` from `hash-only` to `signed` or
   `signed-with-attestation`. This step is manual today, a future
   `perf-sentinel sign` subcommand may automate it.
5. **Publish**: all three files (`report.json`,
   `attestation.intoto.jsonl`, `bundle.sig`) are published at the
   operator's transparency URL.

A consumer downloads the three files and runs
`perf-sentinel verify-hash --report report.json --attestation
attestation.intoto.jsonl --bundle bundle.sig` or, more concisely,
`perf-sentinel verify-hash --url https://example.fr/report.json`
which fetches the sidecars by convention.

## In-toto v1 statement format

The attestation produced by `disclose --emit-attestation` is a
single-statement in-toto v1 document. Shape:

```json
{
  "_type": "https://in-toto.io/Statement/v1",
  "predicateType": "https://perf-sentinel.io/attestation/v1",
  "subject": [
    {
      "name": "report.json",
      "digest": { "sha256": "<64-hex>" }
    }
  ],
  "predicate": {
    "perf_sentinel_version": "0.7.0",
    "report_uuid": "...",
    "period": { "from_date": "2026-01-01", "to_date": "2026-03-31" },
    "intent": "official",
    "confidentiality_level": "public",
    "organisation": {
      "name": "Example SAS",
      "country": "FR",
      "identifiers": { "siren": "...", "domain": "..." }
    },
    "methodology_summary": {
      "sci_specification": "ISO/IEC 21031:2024",
      "conformance": "core-required",
      "calibration_applied": true,
      "period_coverage": 0.91,
      "core_patterns_count": 4,
      "enabled_patterns_count": 10,
      "disabled_patterns_count": 0,
      "core_patterns_hash": "<64-hex SHA-256>"
    }
  }
}
```

`predicateType` uses the `perf-sentinel.io` namespace by convention.
The host is not formally owned by the project today, this is the
standard practice for custom in-toto predicates. Verifiers identify
the predicate by exact string match.

The `subject.digest.sha256` is the SHA-256 of the report file as
written on disk, not the canonical `content_hash` field. The two
serve different purposes: the canonical hash is deterministic
(sorted keys, one field blanked) and lives inside the document;
the subject digest is the file's actual byte-level hash and lives in
the attestation.

The three count fields (`core_patterns_count`,
`enabled_patterns_count`, `disabled_patterns_count`) let a consumer
reading only the signed predicate detect a report that claims
`conformance: "core-required"` while having dropped one of the four
core patterns post-hoc. The invariant `enabled_patterns_count >=
core_patterns_count` is enforced by the validator for `intent =
"official"` (`validate_official` refuses any disclosure where a
core pattern is missing from the enabled set), so every conformant
official disclosure satisfies it by construction.

The `core_patterns_hash` field (SHA-256 over the sorted, colon-joined
names) complements the counts by detecting substitution: an attacker
who replaces `n_plus_one_sql` with `slow_sql` keeps
`core_patterns_count = 4` but changes the hash. A consumer
recomputes the hash over the canonical `core_patterns_required()`
list for the perf-sentinel version recorded in
`perf_sentinel_version` (currently four: `n_plus_one_sql`,
`n_plus_one_http`, `redundant_sql`, `redundant_http`) and compares
it against the signed hash.

`verify-hash` automates this cross-check: it hashes the canonical
core set baked into the local verifying binary, hashes the report's
`methodology.core_patterns_required`, and surfaces a `[FAIL] Core
patterns` line when the two diverge. The check runs on every
`verify-hash` invocation, no extra flag required. A consumer running
the same perf-sentinel version as the signer therefore catches a
substitution attempt without needing an external reference table. A
divergence against a verifying binary on a different version is
surfaced with a hint ("verifying binary is a different perf-sentinel
version") so the consumer can re-run with a matching version.

## Cosign command

For Sigstore public signing with keyless OIDC, the recommended
command for operators is:

```bash
cosign sign-blob \
    --bundle bundle.sig \
    --new-bundle-format \
    attestation.intoto.jsonl
```

The OIDC issuer (browser flow or GitHub Actions token) records the
signer identity in the bundle. Operators using a private Rekor
instance pass `--rekor-url https://rekor.internal.example.fr`
matching their `[reporting.sigstore].rekor_url` config.

**Pitfall to avoid.** Do not use `cosign attest-blob --predicate
attestation.intoto.jsonl ...` here. `attest-blob --predicate` treats
its argument as a raw predicate and wraps it inside a fresh in-toto
v1 Statement on the fly. Since the disclose pipeline already emits
a complete Statement, the result is a Statement-of-Statement that
Rekor records permanently in the public transparency log. Use
`sign-blob` to sign the already-formed Statement as-is, with the
matching `--new-bundle-format` so the bundle carries the Rekor
inclusion proof in the form `verify-blob` expects.

cosign 2.4+ is required for the `--new-bundle-format` flag. Older
cosign versions emit a legacy bundle that `cosign verify-blob`
will reject; operators on cosign <2.4 should upgrade before
signing for transparency.

We deliberately do not support the `--no-tlog-upload` flag in the
verify path: a bundle without a Rekor inclusion proof is rejected
with a clear error message. Public auditability is a property of
the format, not an optional opt-in.

## Verification flow

`perf-sentinel verify-hash` chains up to three checks:

1. **Content hash** (pure Rust, always runs). Recomputes the
   canonical SHA-256 of the report and compares to
   `integrity.content_hash`.
2. **Signature** (delegated to `cosign verify-blob`). Runs
   when `integrity.signature` is present in the report and the
   operator passes `--attestation` and `--bundle` (or `--url` mode
   pulls them automatically).
3. **Binary attestation** (delegated to `gh attestation verify`
   from v0.7.1 onward, `slsa-verifier verify-artifact` on the
   legacy v0.7.0 release). The verify-hash output prints a metadata
   summary and the exact verification command to run against the
   binary downloaded from `integrity.binary_verification_url`. The
   0.7.1 migration moved the attestation storage from a release
   asset (`multiple.intoto.jsonl`) to the GitHub attestations API
   via `actions/attest-build-provenance`. Binary fetch + verify in
   a single command is future work.

Exit codes:

| Code | Meaning                                                          |
|------|------------------------------------------------------------------|
| `0`  | TRUSTED                                                          |
| `1`  | UNTRUSTED (a check returned a hard failure)                      |
| `2`  | PARTIAL (no hard failure, at least one check could not complete) |
| `3`  | INPUT_ERROR                                                      |
| `4`  | NETWORK_ERROR (`--url` mode only)                                |

The split between UNTRUSTED (1) and PARTIAL (2) lets a wrapper
script tell a tamper attempt from a missing tool. A naive
`verify-hash && deploy` gate still rejects PARTIAL because the
exit is non-zero.

## Privacy on Rekor public

Every signature uploaded to Sigstore public Rekor produces a
permanent, world-readable transparency log entry. The entry
contains:

- The signer identity recorded by the OIDC issuer (e.g. a Google
  email, a GitHub Actions workflow URL with org/repo).
- The hash of the signed payload (the in-toto statement here).
- A timestamp.

The entry does not contain the report itself or its content.
Operators concerned about leaking signer identity should consider:

- Using a dedicated service-account email for signing.
- Running a private Rekor instance (`[reporting.sigstore].rekor_url`).
- Signing with a GitHub Actions workflow whose identity URL is
  pre-disclosed by the organisation.

For most public-transparency use cases, leaking the signer
identity is the intended outcome: the consumer wants to know which
identity vouches for the report.

## Failure modes

What a consumer should conclude when each check fails:

- **Content hash FAIL**: the file is corrupted or has been
  tampered with after publication. Untrusted.
- **Signature FAIL** with valid content_hash: the report itself is
  intact but no longer has a valid Sigstore proof. Likely the
  bundle was replaced, the Rekor entry was revoked, or the
  certificate identity does not match the claimed signer.
  Untrusted.
- **Signature SKIP** because `cosign` is not installed: install
  cosign and retry, the report is not necessarily untrusted but
  cannot be verified at the user's current install. Content hash
  by itself is a weaker guarantee.
- **Binary attestation NotProvided**: the report was produced by
  a binary that does not carry SLSA provenance metadata (e.g. a
  local development build). Content hash + Sigstore signature
  still hold, but the consumer cannot verify what produced the
  report.
- **Binary attestation FAIL**: the binary referenced by
  `integrity.binary_verification_url` does not match the SLSA
  attestation, or the source-uri does not match
  `github.com/robintra/perf-sentinel`. Treat as untrusted.

The overall verdict surfaces as one of `TRUSTED` (content hash +
signature both OK), `PARTIAL` (content hash OK but signature
NotProvided or Skip), or `UNTRUSTED` (any FAIL).

## Tooling: `hash-bake`

The `hash-bake` subcommand (0.7.2+) computes the canonical `content_hash` of a report and writes it back into `integrity.content_hash` without going through the full `disclose` pipeline. It exists for test fixture generation and for debugging reports whose hash has drifted from canonical after manual edits. By default it refuses to operate on reports that already carry an `integrity.signature` to avoid masking workflow errors. See `docs/REPORTING.md` § "Computing a canonical content hash with `hash-bake`" for the operator-facing reference.

## Cross-references

- `docs/SCHEMA.md` documents the on-the-wire shape of
  `integrity.signature` and `integrity.binary_attestation`.
- `docs/REPORTING.md` is the operator-facing signing workflow.
- `docs/SUPPLY-CHAIN.md` covers the SLSA generator integration in
  the GitHub Actions release workflow.
- `docs/schemas/perf-sentinel-report-v1.json` carries the
  authoritative JSON Schema definitions.
