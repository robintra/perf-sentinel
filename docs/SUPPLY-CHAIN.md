# Supply chain pinning policy

This document describes how perf-sentinel keeps its build inputs
immutable. The goal is simple: a checkout of any tagged release
produces byte-identical CI runs and binaries weeks or years later,
and a compromised upstream cannot silently swap a tag from under us.

The policy below is already enforced across the repository. This
document formalises it so future contributors and reviewers can apply
the same rules to new workflows, Dockerfiles and dependencies.

## Status

Compliance check at 2026-05-03:

- **GitHub Actions**: 100% of `uses:` lines across the 9 workflows in
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
