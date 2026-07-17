# Supply chain pinning policy

This document describes how perf-sentinel keeps its build inputs
immutable. The goal is simple: a checkout of any tagged release
produces byte-identical CI runs and binaries weeks or years later,
and a compromised upstream cannot silently swap a tag from under us.

The policy below is already enforced across the repository. This
document formalises it so future contributors and reviewers can apply
the same rules to new workflows, Dockerfiles and dependencies.

## Status

Compliance check at 2026-06-09:

- **GitHub Actions**: 100% of `uses:` lines across the 11 workflows in
  `.github/workflows/` are pinned to a 40-character commit SHA, with
  the human-readable tag in a trailing comment.
- **Dockerfile**: the production image is `FROM scratch`, with no
  external base image to pin. The only Docker action invoked from
  CI (`zricethezav/gitleaks` in `ci.yml`) is pinned by digest.
- **Cargo dependencies**: `Cargo.lock` is committed and tracked. The
  workspace runs `cargo audit` daily via
  `.github/workflows/security-audit.yml`. Acknowledged advisories
  with documented exposure analysis live in `audit.toml`.
- **Permissions**: every workflow declares `permissions:` at the job
  level (default `contents: read`), with broader scopes opted into
  per job only where required (release, packages, attestations).
- **Dependabot**: configured for `github-actions` in
  `.github/dependabot.yml`, weekly Monday schedule, grouped by
  upstream owner to keep the diff coherent.

## Pinning rules

### GitHub Actions

Every `uses:` line in a workflow must reference a 40-character commit
SHA. The semver tag goes into a trailing comment so reviewers can
read the version at a glance:

```yaml
- uses: actions/checkout@1af3b93b6815bc44a9784bd300feb67ff0d1eeb3  # v6.0.2
```

Why SHA and not tags: the recent supply-chain attacks against
`tj-actions/changed-files` (March 2025) and similar incidents all
exploited the fact that a Git tag is a mutable pointer. A maintainer
or attacker can move `v6` to a new commit at any time, and every
workflow on the planet that pinned `@v6` immediately runs the new
code. A SHA is content-addressable: rewriting it requires a
collision in SHA-1, which is not in scope for any known attacker.

### Docker images

When a Dockerfile or workflow references an external image, pin the
content digest:

```dockerfile
FROM golang@sha256:abc...def  # 1.22-alpine
```

The production `Dockerfile` uses `FROM scratch`, so there is nothing
to pin in the image itself. The binary copied in (`build/linux-${TARGETARCH}/perf-sentinel`)
is built from this very repository, with `Cargo.lock` driving its
dependency closure.

### Cargo dependencies

- `Cargo.toml` declares semver ranges as usual.
- `Cargo.lock` is committed and is the authoritative source for what
  the build actually compiles.
- `cargo audit` runs daily and on every PR.
- Acknowledged advisories live in `audit.toml` with a paragraph
  explaining why the affected code path is not exercised. See the
  `RUSTSEC-2026-0097` entry for the format and depth expected.

### Workflow permissions

The default `GITHUB_TOKEN` ships with broad permissions. Workflows
explicitly downgrade that to `contents: read` at the job level and
opt in to additional scopes only where required:

```yaml
jobs:
  build:
    permissions:
      contents: read
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@1af3b93b6815bc44a9784bd300feb67ff0d1eeb3
```

Release jobs that need to push to GHCR or create a release add
`packages: write`, `contents: write` or `attestations: write` as
needed. There is no top-level `permissions: write-all` anywhere in
the repository.

## Dependabot configuration

The relevant excerpt from `.github/dependabot.yml`:

```yaml
version: 2
updates:
  - package-ecosystem: "github-actions"
    directory: "/"
    schedule:
      interval: "weekly"
      day: "monday"
    open-pull-requests-limit: 5
    groups:
      ci-actions:
        patterns: ["actions/*", "dtolnay/*", "Swatinem/*", "taiki-e/*", "actions-rust-lang/*"]
      docker-actions:
        patterns: ["docker/*"]
      security-actions:
        patterns: ["github/codeql-action", "github/codeql-action/*"]
```

Cargo dependencies are deliberately excluded from Dependabot: the
combination of `Cargo.lock` plus daily `cargo audit` already covers
the security angle, and the volume of patch bumps Dependabot would
generate on a 200+ crate workspace pays off poorly for a project of
this size. Cargo updates are handled manually via `cargo update`
when needed.

## Verification commands

Run these at any time to audit the repository's pinning posture:

```bash
# 1. Find any GitHub Action whose ref is NOT a 40-char SHA. Expected: 0 hits.
#    Matches anything after `@` that isn't 40 hex characters: semver tags,
#    branch names, `latest`, `HEAD`, custom refs like `release-1.2`.
grep -rnE 'uses:[[:space:]]+[^@]+@[^[:space:]#]+' .github/workflows/ \
  | grep -vE 'uses:[[:space:]]+[^@]+@[a-f0-9]{40}([[:space:]]|$)'

# 2. Find any FROM line in a Dockerfile that is not digest-pinned.
#    Expected: only `FROM scratch` and explicit digests.
grep -rnE '^FROM[[:space:]]+[^@]+:[^@]+$' \
  Dockerfile* charts/*/Dockerfile* 2>/dev/null

# 3. Run cargo audit. Expected: only the documented ignores fire.
cargo audit

# 4. Inspect actions permissions on the repo. Expected: enabled and
#    `selected` (not `all`). Requires gh CLI authenticated.
gh api repos/robintra/perf-sentinel/actions/permissions
```

## Bumping a pin manually

Dependabot handles the routine bumps. When you need to do it by
hand (security update outside the weekly cycle, or a new action that
Dependabot has not yet picked up), resolve the SHA via the GitHub API:

```bash
# Resolve the SHA for a given semver tag of a published action.
TAG="v6.0.2"
gh api repos/actions/checkout/git/ref/tags/${TAG} --jq '.object.sha'
```

Then update the workflow:

```yaml
- uses: actions/checkout@<the-sha-you-just-resolved>  # v6.0.2
```

Always update the trailing comment to match the new tag. A SHA with
a stale comment is worse than no comment.

For Docker images, resolve the digest with `docker buildx`:

```bash
docker buildx imagetools inspect <image>:<tag> --format '{{.Manifest.Digest}}'
```

## CVE response process

1. **Detection**: `cargo audit` runs daily and posts on PRs. GitHub
   Security Advisories surface the same data plus ecosystem-specific
   alerts. Dependabot opens security PRs automatically when a fix is
   available.

2. **Triage**: read the advisory, run `cargo tree -i <crate>` to
   confirm whether the affected version is actually compiled into
   the binary (the `RUSTSEC-2026-0097` paragraph in `audit.toml` is
   the canonical example of what depth of analysis is expected).

3. **Remediation**: bump the dependency in `Cargo.toml` if the fix
   is upstream, run `cargo update -p <crate>`, verify with
   `cargo audit`, open a PR with `chore(deps)` prefix.

4. **Acknowledgment**: if the affected code path is not exercised,
   add an entry to `audit.toml` with a paragraph explaining the
   exposure analysis and the conditions under which the entry should
   be revisited. Do not silently ignore.

5. **Disclosure**: see `SECURITY.md` for the full coordinated
   disclosure process and supported version matrix.

## SLSA build provenance

### Background: Sigstore primer

If you have not used Sigstore before, this short primer is a prerequisite for the SLSA, Cosign, Rekor and in-toto references that follow. Other perf-sentinel docs link back here for canonical definitions, see [docs/REPORTING.md](REPORTING.md#background-sigstore-primer), [docs/METHODOLOGY.md](METHODOLOGY.md#cryptographic-integrity-070), [docs/HELM-DEPLOYMENT.md](HELM-DEPLOYMENT.md#software-supply-chain), [docs/SCHEMA.md](SCHEMA.md#integrity).

**Why Sigstore.** Sigstore is an open-source toolkit hosted by the Open Source Security Foundation (OpenSSF) and maintained by Google, Red Hat, Chainguard, GitHub and the Linux Foundation. It is the de-facto standard for verifiable artefact signatures in the cloud-native ecosystem (Kubernetes, Helm, npm provenance and PyPI attestations all rely on it). perf-sentinel uses it in three places: signing official release binaries (SLSA Build L3 attestation), signing the Helm chart (Cosign signature verifiable via `cosign verify`), and signing periodic disclosure reports (`integrity.signature` with a Rekor inclusion proof). Three properties drive the choice:

1. **Keyless signing**, no long-lived private key for the signer to manage or leak.
2. **A public, tamper-evident log** (Rekor), so a third party can independently verify that a signature existed at a given point in time.
3. **Free, open-source, self-hostable**, no proprietary lock-in or per-signature billing.

**The three components.**

- **Cosign** is the client CLI you run locally (or that GitHub Actions runs in CI). It opens an OIDC flow in your browser, signs the artefact, and ships the signature to Sigstore.
- **Fulcio** is the certificate authority. It consumes the OIDC token cosign obtained (proof of identity: email, GitHub workflow URL, ...) and issues a short-lived X.509 certificate (10 minutes) bound to that identity. Fulcio never sees the signer's private key.
- **Rekor** is the public transparency log. It records the signature next to the Fulcio certificate, returns an inclusion proof, and exposes the entry at a stable log index. Past entries cannot be silently rewritten.

**Who signs with which key.** Cosign generates a brand-new ephemeral keypair just before signing. Fulcio issues a 10-minute certificate that binds the *public* half of that keypair to the OIDC identity. Once the signature is uploaded to Rekor the keypair is discarded. What survives is the signature, the certificate, and the Rekor entry, which is exactly what a verifier needs.

**The OIDC identity** is the subject of the Fulcio certificate, surfaced as `signer_identity` + `signer_issuer` in any document that records the signature. For a GitHub Actions release workflow the identity is the workflow URL (`https://github.com/robintra/perf-sentinel/.github/workflows/release.yml@refs/tags/...`) and the issuer is `https://token.actions.githubusercontent.com`. For an individual signing locally with a Google account, the identity is the email address and the issuer is `https://accounts.google.com`. Consumers should pin the expected identity regex and issuer in their verification policy.

**Known limitation: OIDC issuer migration.** The issuer URL is recorded inside the certificate and therefore in Rekor. If the producing organisation migrates between identity providers later, past signatures remain valid but new signatures will carry a different `signer_issuer`. Verifier policies that pin a specific issuer must be updated, otherwise they reject the new signatures as untrusted. Plan the pinning policy with provider migrations in mind.

**Related terms you will see in perf-sentinel supply-chain commands.** One-liners only, full definitions in the linked specs.

- **OIDC (OpenID Connect)** is an identity protocol layered on OAuth 2.0. In this workflow it is how cosign proves "this signer is `user@example.org`" (or "this is the perf-sentinel release workflow on tag v0.7.1") to Fulcio. [Spec](https://openid.net/specs/openid-connect-core-1_0.html).
- **in-toto v1 statement** is an open OpenSSF specification for software-supply-chain attestations. A JSON envelope that pairs an artefact hash with a typed *claim* about it. SLSA provenance and the periodic disclosure attestation are both in-toto statements internally. Cosign signs the statement, not the raw artefact, so verifiers can chain the trust from artefact hash to in-toto statement to cosign signature to Fulcio cert. [Spec](https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md).
- **Bundle (`bundle.sig`)** is the JSON file cosign writes at sign time. It packs the signature, the Fulcio certificate, and the Rekor inclusion proof into a single artefact, which is what enables fully offline verification later (a consumer validates against Rekor's public key without re-querying Rekor live).
- **SLSA (Supply-chain Levels for Software Artifacts)** is a separate OpenSSF framework that describes *how* an artefact was built (source commit, builder, workflow). perf-sentinel binaries and Helm charts carry SLSA Build L3 attestations produced by `actions/attest-build-provenance`. Level L3 requires Sigstore OIDC signing plus builder isolation, both of which a GitHub-hosted runner provides. [Spec](https://slsa.dev/spec/v1.0/).
- **SBOM (Software Bill of Materials)** is a structured inventory of an artefact's dependencies. perf-sentinel ships an SPDX-format SBOM attested under the SPDX in-toto predicate, so consumers verify it the same way they verify the Cosign signature. [SPDX spec](https://spdx.dev/specifications/), [SPDX in-toto predicate](https://github.com/in-toto/attestation/blob/main/spec/predicates/spdx.md).
- **CT log (Certificate Transparency)** is the broader pattern Rekor implements. Sigstore's Rekor public instance is at `rekor.sigstore.dev`. Operators with stricter requirements can run a private instance.

### Workflow

Starting with v0.7.1, every official perf-sentinel release binary
carries a SLSA Build L3 provenance attestation. The attestation is
generated by GitHub Actions through `actions/attest-build-provenance`
(maintained under the GitHub `actions/` org) and stored on the
GitHub attestations API associated with this repository. It is **not**
published as a release asset.

The 0.7.1 release migrated from the previous tooling,
`slsa-framework/slsa-github-generator@v2.1.0`, which had been in
de-facto maintenance since 2025-02-24 (15 months without a release as
of the migration date, all internal actions still on Node.js 20 while
GitHub-hosted runners switch to Node 24 default on 2 June 2026). The
new pipeline preserves the SLSA Build Provenance contract, drops the
release-asset `multiple.intoto.jsonl` (attestations now live in the
attestations API), and upgrades the level claim from L2 to L3 since
`actions/attest-build-provenance` produces a level-3 attestation by
construction (provenance signed via Sigstore OIDC, builder isolation
on a GitHub-hosted runner).

Verify a downloaded binary:

```bash
gh attestation verify perf-sentinel-linux-amd64 \
  --owner robintra \
  --repo perf-sentinel
```

A successful verification confirms that the binary was built from a
tagged release of this repository by GitHub Actions, not by a third
party. Combine with the `verify-hash` subcommand against a periodic
disclosure report to verify the full chain:
`source -> SLSA -> binary -> report -> Sigstore signature`.

**Prerequisite**: `gh` CLI 2.49+ on the consumer side (earlier
versions do not implement `gh attestation verify`). The same
verification can be performed via the Sigstore client SDKs directly
against the GitHub attestations API for tooling that cannot depend on
`gh`.

**Migration note for consumers**: a 0.6.x or 0.7.0 binary still ships
the legacy `multiple.intoto.jsonl` and is verified via
`slsa-verifier verify-artifact`. The legacy verification path is
preserved on those existing tags; only 0.7.1+ requires the new
command.

## Binary SBOM and embedded audit data

Beyond SLSA provenance, every binary release carries two more supply-chain
artefacts, in the same shape as the Helm chart's SBOM.

**Embedded `cargo-auditable` data.** Each release binary is built with
`cargo auditable build`, so its resolved dependency list is embedded in the
binary itself. Audit the shipped artefact directly, not just the repo's
`Cargo.lock`:

```bash
cargo audit bin perf-sentinel-linux-amd64
```

**SPDX SBOM.** Each release ships `perf-sentinel-sbom.spdx.json`, an SPDX SBOM
that Syft derives from that embedded data and attests under the SPDX predicate
(`https://spdx.dev/Document/v2.3`) against the Linux amd64 binary, the same
predicate the chart uses. Verify the attestation against the binary (the SBOM
is the attestation's predicate, the binary is its subject):

```bash
gh attestation verify perf-sentinel-linux-amd64 \
  --repo robintra/perf-sentinel \
  --predicate-type https://spdx.dev/Document/v2.3
```

The SBOM is derived from the Linux amd64 binary; the four release binaries
share their Rust dependency closure bar a few platform-shim crates, so it
documents the release as a whole.

## PR review checklist

When reviewing a PR that touches CI infrastructure:

- New `uses:` line? Must pin a 40-character SHA, tag in trailing
  comment.
- New `FROM` line in a Dockerfile? Must pin `image@sha256:<digest>`,
  unless `FROM scratch`.
- New Cargo dependency? `cargo audit` must pass on the PR. If a new
  advisory is unavoidable, the contributor must add an `audit.toml`
  entry with the same depth of analysis as the existing entries.
- New workflow? `permissions:` block at the job level, default to
  `contents: read`, opt in to broader scopes only where required.
- Top-level `permissions: write-all`? Reject. Use job-level scopes
  instead.

The verification commands above can be run locally before pushing
to make sure the PR is clean.
