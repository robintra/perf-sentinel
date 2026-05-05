# perf-sentinel CI guide

CI-side integration: run perf-sentinel in batch mode against a trace fixture produced by your integration test stage, and surface the findings on every pull request. For topology overviews see [`INTEGRATION.md`](./INTEGRATION.md), for application-side instrumentation see [`INSTRUMENTATION.md`](./INSTRUMENTATION.md).

## Contents

- [CI mode (batch analysis)](#ci-mode-batch-analysis): the underlying CLI invocation and exit-code semantics behind every recipe below.
- [CI integration recipes](#ci-integration-recipes): copy-pasteable templates for GitHub Actions, GitLab CI and Jenkins, plus the quality-gate philosophy and the interactive HTML report path for each provider.
- [PR regression detection (`diff` subcommand)](#pr-regression-detection-diff-subcommand): compare a PR trace set against a baseline trace set to flag regressions.

## CI mode (batch analysis)

For CI pipelines, use batch mode instead of daemon mode:

```bash
perf-sentinel analyze --ci --input traces.json
```

Exit code is non-zero if the quality gate fails. Configure thresholds in `.perf-sentinel.toml`:

```toml
[thresholds]
n_plus_one_sql_critical_max = 0
n_plus_one_http_warning_max = 3
io_waste_ratio_max = 0.30
```

---

## CI integration recipes

Ready-to-copy templates for the three major CI providers live in
[`docs/ci-templates/`](./ci-templates/). Pick the one that matches your
provider, drop it into your repository, adapt the three variables called out
in the template's leading comment block (version pin, trace path, config
path) and you are done.

| Provider       | Template                                                  | What it surfaces                                  |
|----------------|-----------------------------------------------------------|---------------------------------------------------|
| GitHub Actions | [`github-actions.yml`](./ci-templates/github-actions.yml) | SARIF in GitHub Code Scanning + sticky PR comment |
| GitLab CI      | [`gitlab-ci.yml`](./ci-templates/gitlab-ci.yml)           | SARIF artifact + Code Quality widget on the MR    |
| Jenkins        | [`jenkinsfile.groovy`](./ci-templates/jenkinsfile.groovy) | Warnings Next Generation issue tree + trend chart |

### Quality-gate philosophy

All three templates run `perf-sentinel analyze --ci` as the gating step. The `--ci` flag exits with code `1` when any threshold in `[thresholds]` is breached. The templates translate that exit code differently based on the trigger:

| Trigger | Behavior | Rationale |
|---------|----------|-----------|
| Pull request | Gate blocks (red build) | Author is still in context, cost of correction is lowest |
| Push to trunk | Gate is informational only, SARIF still uploaded | A merged commit should not be held up by perf-sentinel between merge and release |

This split avoids the common failure mode where PR-gates that also enforce on trunk leave main red, the team works around it, and the tool gets disabled.

The recommended setup runs perf-sentinel twice in the same job: once without `--ci` (always produces a SARIF artifact for reviewer inspection) and once with `--ci` (enforces the gate). The Jenkins and GitLab templates do this explicitly, the GitHub template uses `continue-on-error` for the same effect in one invocation.

Per-provider PR-vs-trunk wiring:

- **GitHub Actions**: PR step runs when `github.event_name == 'pull_request'` and calls `exit 1` on breach, trunk step emits a `::warning::` annotation without failing.
- **GitLab CI**: `allow_failure: true` on the `$CI_COMMIT_BRANCH == $CI_DEFAULT_BRANCH` rule. The job still returns exit 1 on breach, the pipeline badge stays green, the job shows a yellow warning icon.
- **Jenkins**: `when { expression { env.CHANGE_ID != null } }` on the `Quality gate (PR only)` stage. `CHANGE_ID` is populated by MultiBranch Pipeline only on PRs, so branch builds skip the stage. The Warnings NG `qualityGates` follows the same guard.

### Interactive report via GitHub Pages

The sticky PR comment (markdown block with finding counts and quality
gate status) gives reviewers an at-a-glance view. For a deeper
inspection (span tree with highlighted N+1s, framework-specific
suggested fixes, pg_stat drill-down, full Diff against trunk), the
GitHub Actions template optionally publishes a **full HTML dashboard**
to GitHub Pages on every PR, linked from the sticky comment as:

> 📊 **Interactive report (Diff view)** → `https://<owner>.github.io/<repo>/perf-sentinel-reports/pr-<N>/index.html#diff`

Clicking the link opens the report on the Diff tab, which is the
natural view for a reviewer: new findings introduced by the PR,
resolved findings (regressions fixed), severity changes, and
endpoint-level I/O metric deltas. The other tabs (Findings, Explain,
pg_stat, Correlations, GreenOps) are one click away via the tab strip.

The reports are self-contained single-file HTML with deep-link hash
routing, so sharing a specific finding is as simple as copying the
URL from the address bar.

**GitHub Pages tier requirement**. On a personal GitHub Free account,
Pages is only available for public repositories. Private repositories
need GitHub Pro, Team, or Enterprise Cloud. See
[GitHub's plans](https://docs.github.com/en/get-started/learning-about-github/githubs-products)
for the current list. If you try to enable Pages on a private repo
with a Free account, the branch push succeeds but Pages serves 404
permanently with no error in the Actions log. Either upgrade the
account, make the repository public, or skip the Pages block and stay
on the SARIF + markdown sticky comment mode.

**Setup** (opt-in, requires GitHub Pages on the repository):

1. Create an empty `gh-pages` branch in the repo (one-time, standard
   GitHub Pages bootstrap).
2. Enable GitHub Pages in `Settings -> Pages`, source = `gh-pages`
   branch, folder = `/ (root)`.
3. Copy the companion baseline workflow from
   [`docs/ci-templates/github-actions-baseline.yml`](./ci-templates/github-actions-baseline.yml)
   to `.github/workflows/perf-sentinel-baseline.yml`. It runs on every
   push to `main` and stores the baseline report under
   `gh-pages/perf-sentinel-reports/baseline.json`.
4. Copy the cleanup workflow from
   [`docs/ci-templates/github-actions-report-cleanup.yml`](./ci-templates/github-actions-report-cleanup.yml)
   to `.github/workflows/perf-sentinel-report-cleanup.yml`. It runs on
   PR close and removes the per-PR directory.
5. Uncomment the `Download baseline from gh-pages`, `Generate
   interactive HTML report`, `Checkout gh-pages worktree` and
   `Publish report to gh-pages` blocks in your main workflow (the
   header comment in
   [`docs/ci-templates/github-actions.yml`](./ci-templates/github-actions.yml)
   locates them).

Once the three workflows are in place, every PR gets its own
interactive report at a stable URL:

```
https://<owner>.github.io/<repo>/perf-sentinel-reports/pr-<N>/
```

The baseline is refreshed on every push to `main`, so the Diff tab
always compares the PR's traces against the latest merged state.

If GitHub Pages is not enabled, the template falls back to the
markdown-only sticky comment. No behaviour change for existing
adopters.

**Fork PR limitations**. The `Post PR comment` step is marked
`continue-on-error: true` because fork PRs receive a read-only
`GITHUB_TOKEN` regardless of the workflow's `permissions:` block.
Without the tolerance, every fork PR would turn the CI red at the
sticky-comment step even when the rest of the pipeline succeeded.
With the tolerance in place, fork PRs still upload SARIF findings to
the Security tab and the Checks UI shows the quality gate result, but
no sticky comment appears on the PR conversation. Same-repo PRs
(internal contributors, same org) keep the full experience, sticky
comment included. Projects where the sticky comment on fork PRs is a
hard requirement should migrate to the `pull_request_target` +
`workflow_run` split documented by [GitHub Security Lab](https://securitylab.github.com/research/github-actions-preventing-pwn-requests/).
That pattern splits the pipeline into a read-only workflow that
builds and uploads artifacts and a write-enabled workflow triggered
by `workflow_run` that downloads those artifacts and posts the
comment. It is not the default in this template because it doubles
the YAML surface and needs careful artifact passing, not proportional
for a getting-started template.

**Concurrency trade-off**. The `concurrency.group: gh-pages-deploy`
guard serializes runs of this workflow against the baseline and
cleanup workflows, so three PRs closed in the same minute cannot
race each other on gh-pages. Because the guard is declared at
workflow scope, it also serializes runs that would not touch Pages
(for example when the Pages blocks are commented out). Repositories
with heavy PR throughput can split the Pages-related steps into a
dedicated job and narrow the concurrency to that job only. Skipped
here to keep the template compact.

**Dependencies**. The deploy uses plain `git` against the `gh-pages`
branch, authenticated with the built-in `GITHUB_TOKEN` and the
`contents: write` permission declared at the workflow level. No
third-party deploy action is required, which keeps the template free
of supply-chain surface for the upload path. Only
`actions/checkout` (pinned) is reused across all three workflows.

**Storage footprint**. A typical report is 80 to 150 KB. With retention
handled by the cleanup workflow, the gh-pages branch only carries
reports for open PRs plus the single `baseline.json`. No unbounded
growth.

**Other providers**. See "Interactive report via GitLab Pages" and
"Interactive report via Jenkins HTML Publisher" below.

### Interactive report via GitLab Pages

Equivalent to the GitHub Pages path above, adapted to GitLab's native
deployment surface. Two template blocks are provided in
[`docs/ci-templates/gitlab-ci.yml`](./ci-templates/gitlab-ci.yml),
pick the one matching your GitLab tier.

**Tier note**. The per-MR deployment mode (`pages.path_prefix`) is
documented as [Experiment, Tier: Premium or
Ultimate](https://docs.gitlab.com/user/project/pages/#create-multiple-deployments),
and is not available on gitlab.com Free. On Free, the MR deployment
appears successful in the environments list but is not actually
served. A Free-tier compatible fallback is provided alongside.

| Block | Tier | Behavior |
| --- | --- | --- |
| `perf-sentinel-pages-simple` | Free | Single deployment on the default branch. Publishes the trunk snapshot of the report AND the baseline JSON at the project Pages root. MR reviewers see the trunk view, not their own MR's analysis. |
| `perf-sentinel-pages` | Premium or Ultimate | One deployment per MR under path prefix `mr-<IID>`, 30-day auto-expiry via `expire_in`. Baseline on the default branch at the Pages root. Native "View deployment" button on the MR UI. |

Pick either block, not both (they would fight over the root deployment).

**Setup** (opt-in, requires GitLab Pages enabled on the project):

1. Enable GitLab Pages under `Settings -> Pages` if not already on.
2. Uncomment exactly one block in
   [`docs/ci-templates/gitlab-ci.yml`](./ci-templates/gitlab-ci.yml).
   Both run in the `perf-sentinel` stage and reuse
   `PERF_SENTINEL_VERSION / PERF_SENTINEL_TRACES / PERF_SENTINEL_CONFIG`
   already declared for the main job.
3. For `perf-sentinel-pages`, confirm GitLab 17.9 or later. Not
   required for `perf-sentinel-pages-simple`.

**Behavior of `perf-sentinel-pages` (Premium or Ultimate)**. The job
differentiates two trigger paths via its `rules:` block:

- **On merge request** (`$CI_PIPELINE_SOURCE == "merge_request_event"`),
  fetches the trunk baseline from the project Pages root (strips the
  MR prefix from `CI_PAGES_URL` via `${CI_PAGES_URL%/mr-[0-9]*}`,
  silent 404 fallback when absent), produces `public/index.html` via
  `perf-sentinel report --output public/index.html`, deploys with
  `path_prefix: "mr-${CI_MERGE_REQUEST_IID}"` and
  `pages.expire_in: 30 days`. `environment.url` points to the active
  `${CI_PAGES_URL}`, which GitLab resolves to the MR-scoped deployment
  URL at runtime.
- **On push to the default branch**, produces
  `public/perf-sentinel-reports/baseline.json` via
  `perf-sentinel analyze --format json`, deploys with an empty
  `path_prefix` so the file lands at the site root and future MR
  deployments can fetch it.

**Behavior of `perf-sentinel-pages-simple` (Free)**. Runs only on the
default branch. Writes both `public/index.html` (the interactive
trunk snapshot) and `public/perf-sentinel-reports/baseline.json` in
one pass, then deploys a single Pages site at the project root.

**Retention**. `perf-sentinel-pages` delegates retention to GitLab.
Parallel deployments are deleted immediately when the MR is closed or
merged. The `pages.expire_in: 30 days` on the template is a backstop
for stale-open MRs (GitLab's default is 24 hours when unset, which we
widen so a long-running MR keeps its live report). Setting
`expire_in: never` disables time-based expiry entirely and relies on
close/merge events only. Use `never` only if your team reliably closes
or merges MRs, otherwise abandoned MRs accumulate until the quota cap
kicks in. `perf-sentinel-pages-simple` has no retention concern, it
keeps a single deployment that is overwritten on every default-branch
push.

**Quota**. gitlab.com allows up to 100 additional parallel deployments
on Premium and 500 on Ultimate, tracked per namespace on top of the
main deployment. Self-managed instances expose the limit through admin
configuration. `perf-sentinel-pages-simple` is a single deployment,
not subject to this cap. For projects running near the cap on
`perf-sentinel-pages`, `expire_in` can be lowered or MRs should be
closed/merged promptly to release slots.

**Storage footprint**. A typical report is 80 to 150 KB and a
baseline JSON is 10 to 50 KB. With retention active on the
Premium path, only open MRs plus the current baseline consume space.
The Free path stores a single deployment.

**Dependencies**. No third-party GitLab CI component. The job uses
`curl` to install the pinned perf-sentinel release binary and the
built-in `pages:` keyword for deployment. No deploy token or runner
token beyond the default `CI_JOB_TOKEN` is required.

### Interactive report via Jenkins HTML Publisher

Equivalent to the GitHub and GitLab paths above, adapted to the
[HTML Publisher plugin](https://plugins.jenkins.io/htmlpublisher/)
that is pre-installed on most enterprise Jenkins. The plugin exposes
the report at a stable URL `${BUILD_URL}perf-sentinel/` and adds a
"perf-sentinel" link in the build sidebar, next to the Warnings NG
report already configured by the template.

Opening that link drops the reviewer into the Findings tab (the
default landing when no baseline is wired, see the Diff tab note
below). The five other tabs (Explain, pg_stat, Correlations,
GreenOps, and a greyed-out Diff tab) are one click away via the
tab strip.

**Jenkins pipeline requirements**:

- Use a **MultiBranch Pipeline** with a branch-source plugin
  installed (GitHub Branch Source, Bitbucket Branch Source, GitLab
  Branch Source, or Gitea Branch Source). The `env.CHANGE_ID` check
  that gates the quality-gate stage on PR builds is only set by
  these plugins. Inside a classic single-branch Pipeline,
  `CHANGE_ID` is always null and the quality gate never blocks.
- Use a **Linux agent** (or a controller without agents on a Linux
  host). The template relies on `sh`, `curl`, `sha256sum`, `chmod`,
  none of which are available on Windows agents by default.

**Setup** (opt-in, requires the HTML Publisher plugin on the
controller):

1. Confirm the HTML Publisher plugin (>= 1.10 for CSP compatibility)
   is installed. Manage Jenkins -> Plugins -> Installed plugins,
   search for "HTML Publisher". If missing, install and restart
   the controller. The Warnings Next Generation plugin used by the
   rest of the template needs to be at >= 9.11.0 for the SARIF tool.
2. Uncomment the `Generate interactive HTML report` stage in
   [`docs/ci-templates/jenkinsfile.groovy`](./ci-templates/jenkinsfile.groovy),
   placed right before the `Quality gate (PR only)` stage.
3. Uncomment the `publishHTML([...])` block in the `post { always }`
   section of the same file. It is paired with the stage above so
   both need to be enabled together for the link to appear.

Once enabled, every build (branch or pull request) produces a
report available at
`${JENKINS_URL}/job/<job-name>/<build-number>/perf-sentinel/`. The
build sidebar carries a "perf-sentinel" link that always points to
the newest build's report via `alwaysLinkToLastBuild: true`. The
`keepAll: true` option retains per-build reports so historical
builds remain browsable.

If the report renders unstyled with broken tab navigation, see
**Configuring Jenkins to render the interactive report** below.
Jenkins applies a strict default Content Security Policy that
blocks inline CSS and JavaScript, which is the most common cause
of an unstyled perf-sentinel sidebar page.

**Configuring Jenkins to render the interactive report**.

Jenkins applies a strict
[Content Security Policy](https://www.jenkins.io/doc/book/security/configuring-content-security-policy/)
by default to content served from build workspaces. The
perf-sentinel HTML report packs CSS and JavaScript inline in a
single self-contained file, which the default CSP blocks. Without
relaxing the policy or using a Resource Root URL, clicking the
`${BUILD_URL}perf-sentinel/` sidebar link shows an unstyled HTML
page with broken tab navigation and no message in the build log.

Two options to fix, in order of preference:

**Option A: configure a Resource Root URL** (Jenkins 2.200+,
recommended). Serves user-generated content from a separate domain
so the main instance CSP no longer applies. Set the URL in
`Manage Jenkins > System > Resource Root URL`. See the
[inline help](https://www.jenkins.io/doc/book/security/user-content/#resource-root-url)
for details. No template change required, all reports across all
jobs benefit immediately.

**Option B: relax the CSP** (legacy, broader scope). Set the
following Java system property on the Jenkins controller startup
(or run it once via the Script Console for a session-scoped
experiment):

```groovy
System.setProperty(
    "hudson.model.DirectoryBrowserSupport.CSP",
    "sandbox allow-scripts; default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline';"
)
```

Tradeoffs:

- Affects all HTML content served by all jobs on the instance, not
  just perf-sentinel reports.
- Adds `'unsafe-inline'` for both styles and scripts. Acceptable on
  a Jenkins instance where you trust the jobs being run, risky on a
  multi-tenant instance with untrusted contributors.
- Reverts to default on Jenkins restart unless persisted via the
  startup options (`JAVA_OPTS`, `jenkins.xml`, or systemd unit).

A future perf-sentinel release may produce a CSP-friendly report
(CSS and JavaScript split into sibling files) that works on the
default Jenkins CSP. No date committed.

**Diff tab absent by default**. Unlike GitHub Actions and GitLab CI
where a companion baseline workflow refreshes `baseline.json` on
every push to the default branch, this template does not wire a
Jenkins baseline. The Diff tab of the report is therefore empty.
Users who want the Diff tab can extend the template with the
[Copy Artifact plugin](https://plugins.jenkins.io/copyartifact/) to
pull a `baseline.json` from the last successful build of the
default-branch job, then pass it to
`perf-sentinel report --before baseline.json`. This enhancement is
out of scope for this template.

**No PR comment posting**. Jenkins does not have a native
pull-request comment mechanism equivalent to GitHub's sticky
comment or GitLab's Code Quality widget. Reviewers who follow a
Jenkins build consult the build page directly, same pattern as for
Warnings NG findings. Teams who want a PR comment can wire the
`gh` CLI or a forge-specific REST API from within the pipeline,
but that requires managing a forge token in Jenkins credentials
and is out of scope for this template.

**Storage footprint** is per-build and retained indefinitely
(`keepAll: true`). A typical report is 80 to 150 KB. For long-lived
Jenkins controllers with high build volume, pair
`publishHTML keepAll: true` with the build discarder in the job
configuration (e.g. keep last N builds) to cap the footprint.

### Where SARIF surfaces in each provider

- **GitHub Code Scanning** lists each finding under the Security tab of the
  repository, with inline source annotations on the PR diff when the
  `code_location` field is present. Requires `permissions.security-events:
  write` on the workflow.
- **GitLab Code Quality** widget shows up on the merge request page, with
  severity colors derived from the perf-sentinel `severity` field
  (`critical -> critical`, `warning -> major`, `info -> info`).
- **Jenkins Warnings Next Generation** publishes a structured issue tree
  with a trend chart per build. The plugin natively understands SARIF
  v2.1.0 and supports its own `qualityGates` declaration as a defense in
  depth on top of the perf-sentinel `--ci` exit code.

---

## PR regression detection (`diff` subcommand)

The `diff` subcommand compares two trace sets and emits a delta report listing new findings, resolved findings, severity changes and per-endpoint I/O op count deltas. The natural fit is a PR check that compares the PR branch's traces against the base branch's traces.

```yaml
# .github/workflows/perf-sentinel-diff.yml
name: perf-sentinel diff

on:
  pull_request:
    branches: [main]

permissions:
  contents: read
  pull-requests: write

jobs:
  diff:
    runs-on: ubuntu-latest
    env:
      PERF_SENTINEL_VERSION: "0.5.8"
    steps:
      - uses: actions/checkout@b4ffde65f46336ab88eb53be808477a3936bae11 # v4.1.1
        with:
          fetch-depth: 0

      - name: Install perf-sentinel
        run: |
          set -euo pipefail
          BASE_URL="https://github.com/robintra/perf-sentinel/releases/download/v${PERF_SENTINEL_VERSION}"
          curl -sSLf -o perf-sentinel-linux-amd64 "${BASE_URL}/perf-sentinel-linux-amd64"
          curl -sSLf -o SHA256SUMS.txt            "${BASE_URL}/SHA256SUMS.txt"
          grep 'perf-sentinel-linux-amd64' SHA256SUMS.txt | sha256sum -c -
          mkdir -p "${GITHUB_WORKSPACE}/bin"
          install -m 0755 perf-sentinel-linux-amd64 "${GITHUB_WORKSPACE}/bin/perf-sentinel"
          echo "${GITHUB_WORKSPACE}/bin" >> "${GITHUB_PATH}"

      # Run integration tests on the PR branch and capture traces.
      - name: Collect PR-branch traces
        run: ./scripts/run-integration-tests.sh
        env:
          OTEL_EXPORTER_OTLP_FILE_PATH: pr-traces.json

      # Re-run on the base branch.
      - name: Collect base-branch traces
        run: |
          git checkout ${{ github.event.pull_request.base.sha }} -- .
          ./scripts/run-integration-tests.sh
        env:
          OTEL_EXPORTER_OTLP_FILE_PATH: base-traces.json

      - name: Diff
        run: |
          perf-sentinel diff \
            --before base-traces.json \
            --after pr-traces.json \
            --config .perf-sentinel.toml \
            --format json \
            --output diff.json
          # SARIF for GitHub Code Scanning (only new findings).
          perf-sentinel diff \
            --before base-traces.json \
            --after pr-traces.json \
            --config .perf-sentinel.toml \
            --format sarif \
            --output diff.sarif

      - name: Upload SARIF
        if: hashFiles('diff.sarif') != ''
        uses: github/codeql-action/upload-sarif@95e58e9a2cdfd71adc6e0353d5c52f41a045d225 # v4.35.2
        with:
          sarif_file: diff.sarif
          category: perf-sentinel-diff

      - name: Comment regression summary on PR
        run: |
          NEW=$(jq '.new_findings | length' diff.json)
          RESOLVED=$(jq '.resolved_findings | length' diff.json)
          REGRESSIONS=$(jq '[.severity_changes[] | select(.after_severity == "critical" or (.after_severity == "warning" and .before_severity == "info"))] | length' diff.json)
          {
            echo "## perf-sentinel diff vs base"
            echo
            echo "- $NEW new finding(s)"
            echo "- $RESOLVED resolved finding(s)"
            echo "- $REGRESSIONS severity regression(s)"
          } > pr-comment.md

      - uses: marocchino/sticky-pull-request-comment@0ea0beb66eb9baf113663a64ec522f60e49231c0 # v3.0.4
        with:
          header: perf-sentinel-diff
          path: pr-comment.md

      - name: Fail on regression
        run: |
          NEW=$(jq '.new_findings | length' diff.json)
          REGRESSIONS=$(jq '[.severity_changes[] | select(.after_severity == "critical")] | length' diff.json)
          if [ "$NEW" -gt 0 ] || [ "$REGRESSIONS" -gt 0 ]; then
            echo "::error::diff introduces $NEW new finding(s) and $REGRESSIONS critical regression(s)"
            exit 1
          fi
```

Tweak the threshold logic in the final step to match your team's policy. Some teams gate on any new finding, others tolerate Info-level new findings and only fail on Warning or Critical regressions.

---

