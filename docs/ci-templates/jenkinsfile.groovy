// perf-sentinel Jenkins declarative pipeline
//
// Runs perf-sentinel in batch mode against an integration-test trace fixture,
// publishes SARIF findings via the Warnings Next Generation plugin, and
// archives JSON + SARIF artifacts. The quality gate is enforced by
// `perf-sentinel analyze --ci` (non-zero exit on threshold breach) and
// duplicated in the Warnings NG `qualityGates` for double safety.
//
// Pipeline type requirement: this template targets MultiBranch Pipelines
// (the standard for repos with PR-based workflows). The `env.CHANGE_ID`
// check that gates the quality-gate stage on PR builds is only set by
// the MultiBranch Pipeline plus a branch-source plugin (GitHub Branch
// Source, Bitbucket Branch Source, GitLab Branch Source, Gitea Branch
// Source). Inside a classic single-branch Pipeline, `CHANGE_ID` is
// always null and the quality gate never blocks. See CI.md section
// "Interactive report via Jenkins HTML Publisher" for details.
//
// What you must adapt before using this template:
//   1. PERF_SENTINEL_VERSION: pin to an exact release tag (never use
//      'latest'). Bump deliberately and review the CHANGELOG before each bump.
//   2. PERF_SENTINEL_TRACES: path to a trace file produced by your
//      integration test stage. The Java reference setup uses an OTel Java
//      Agent with the file exporter writing to target/traces.json.
//   3. PERF_SENTINEL_CONFIG: path to your .perf-sentinel.toml. Tune the
//      [thresholds] section to set quality-gate severity floors.
//
// Required Jenkins plugins:
//   - Warnings Next Generation >= 9.11.0 (publishes SARIF as a structured
//                               issue tree. v9.11.0 introduced the SARIF
//                               tool, earlier versions throw
//                               NoSuchMethodError on `recordIssues`. See
//                               https://plugins.jenkins.io/warnings-ng/releases/)
//   - Pipeline Utility Steps   (only if you want to readJSON the report)
//   - HTML Publisher >= 1.10   (optional, enables the interactive HTML
//                               report block below via publishHTML;
//                               version 1.10+ is CSP-compatible, earlier
//                               versions break in modern Jenkins instances.
//                               Pre-installed on most enterprise Jenkins.)
//
// See docs/CI.md (English) or docs/FR/CI-FR.md (French) for the full CI
// integration guide and the quality-gate philosophy.

pipeline {
    // The template uses Linux shell commands (curl, sha256sum, chmod, sh).
    // On Jenkins controllers with mixed Linux/Windows agents, pinning a
    // Linux label avoids landing on a Windows executor where these
    // commands are unavailable. Rename the label to match your own
    // controller setup. On a single Linux-only controller with no
    // labels configured, replace this line with `agent any` so the
    // build does not queue forever waiting for a label that never
    // matches.
    agent { label 'linux' }

    options {
        // 30 minutes is plenty for a perf-sentinel run on a typical
        // trace fixture. Adjust if your integration test stage takes
        // longer to produce $PERF_SENTINEL_TRACES.
        timeout(time: 30, unit: 'MINUTES')
        // Avoid concurrent runs on the same branch overwriting each
        // other's archived artifacts. The default behavior queues new
        // builds behind the running one. For teams iterating fast on
        // PR feedback, replace with `disableConcurrentBuilds(abortPrevious: true)`
        // so each new push aborts the in-flight build instead of
        // queuing.
        disableConcurrentBuilds()
    }

    environment {
        PERF_SENTINEL_VERSION = '0.7.2'
        PERF_SENTINEL_TRACES  = 'target/traces.json'
        PERF_SENTINEL_CONFIG  = '.perf-sentinel.toml'
    }

    stages {
        // Place your integration-test stage here. It must produce the trace
        // file at $PERF_SENTINEL_TRACES before the perf-sentinel stage runs.
        //
        // stage('Integration tests') {
        //     steps {
        //         sh 'mvn verify -DskipUnitTests=false'
        //     }
        // }

        stage('Install perf-sentinel') {
            steps {
                sh '''
                    set -euo pipefail
                    BASE_URL="https://github.com/robintra/perf-sentinel/releases/download/v${PERF_SENTINEL_VERSION}"
                    curl -sSLf -o perf-sentinel-linux-amd64 "${BASE_URL}/perf-sentinel-linux-amd64"
                    curl -sSLf -o SHA256SUMS.txt            "${BASE_URL}/SHA256SUMS.txt"
                    # Verify integrity before executing. Fails the build if
                    # the binary was tampered with or the release is corrupted.
                    grep 'perf-sentinel-linux-amd64' SHA256SUMS.txt | sha256sum -c -
                    mv perf-sentinel-linux-amd64 perf-sentinel
                    chmod +x perf-sentinel
                    ./perf-sentinel --version
                '''
            }
        }

        stage('perf-sentinel analyze') {
            steps {
                // SARIF artifact for Warnings NG and downstream archival.
                // Always produced (no --ci) so the report exists even when
                // the gate would fail.
                sh '''
                    set -euo pipefail
                    ./perf-sentinel analyze \\
                        --input ${PERF_SENTINEL_TRACES} \\
                        --config ${PERF_SENTINEL_CONFIG} \\
                        --format sarif > perf-sentinel-results.sarif

                    ./perf-sentinel analyze \\
                        --input ${PERF_SENTINEL_TRACES} \\
                        --config ${PERF_SENTINEL_CONFIG} \\
                        --format json > perf-sentinel-report.json
                '''
            }
        }

        // Optional: produce the interactive HTML dashboard that the
        // HTML Publisher plugin exposes under `${BUILD_URL}perf-sentinel/`.
        // Works on both branch and pull-request builds. The Diff tab
        // of the report is absent on Jenkins by default because this
        // template does not wire a baseline; the other tabs (Findings,
        // Explain, pg_stat, Correlations, GreenOps) render normally.
        // See docs/CI.md "Interactive report via Jenkins HTML
        // Publisher" for the full setup and the baseline enhancement
        // path via the Copy Artifact plugin.
        //
        // stage('Generate interactive HTML report') {
        //     steps {
        //         sh '''
        //             set -euo pipefail
        //             ./perf-sentinel report \\
        //                 --input ${PERF_SENTINEL_TRACES} \\
        //                 --config ${PERF_SENTINEL_CONFIG} \\
        //                 --output report.html
        //         '''
        //     }
        // }

        stage('Quality gate (PR only)') {
            // Philosophy: the gate blocks when the build was triggered
            // by a pull request so the developer still has a chance
            // to fix before merge, but never blocks a branch build.
            // The archived SARIF + Warnings NG publication below
            // carry the signal for trunk runs. A red pipeline on
            // main would only keep the default branch red,
            // demotivate the team, and eventually push them to
            // disable this stage. `env.CHANGE_ID` is set by
            // MultiBranch Pipeline only for pull-request builds.
            when { expression { env.CHANGE_ID != null } }
            steps {
                // Re-run with --ci to enforce thresholds. Exit code
                // 1 fails the stage and the build goes red. On branch
                // builds (env.CHANGE_ID == null) this stage is
                // skipped and the build stays green.
                sh '''
                    set -euo pipefail
                    ./perf-sentinel analyze \\
                        --ci \\
                        --input ${PERF_SENTINEL_TRACES} \\
                        --config ${PERF_SENTINEL_CONFIG}
                '''
            }
        }
    }

    post {
        always {
            archiveArtifacts(
                artifacts: 'perf-sentinel-report.json, perf-sentinel-results.sarif, target/traces.json',
                allowEmptyArchive: true
            )
            // Publish SARIF via Warnings NG. On pull-request builds
            // the attached qualityGates also fails the build on any
            // ERROR-level issue, providing defense-in-depth for the
            // perf-sentinel --ci stage.
            recordIssues(
                // enabledForFailure: true ensures Warnings NG processes
                // the SARIF report even when the build was marked
                // FAILURE by the earlier Quality gate stage. Without
                // this, the panel would be empty on PR builds that
                // exceed thresholds, the most valuable case for
                // reviewers.
                enabledForFailure: true,
                tools: [
                    sarif(
                        pattern: 'perf-sentinel-results.sarif',
                        id: 'perf-sentinel',
                        name: 'perf-sentinel'
                    )
                ],
                // Philosophy: the Warnings NG quality gate only
                // activates on pull-request builds. On branch builds
                // the SARIF is still published for dashboarding but
                // does not fail the pipeline.
                qualityGates: env.CHANGE_ID != null ? [
                    [threshold: 1, type: 'TOTAL_ERROR', criticality: 'FAILURE']
                ] : []
            )
            // Optional: expose the interactive HTML report produced
            // by the "Generate interactive HTML report" stage above
            // at a stable URL `${BUILD_URL}perf-sentinel/` on the
            // build page, alongside Warnings NG. Requires the
            // HTML Publisher plugin listed in the header.
            //
            // Enable both the stage above and this block together,
            // uncommenting only one leaves the sidebar pointing at
            // an empty report. `allowMissing: true` keeps this step
            // tolerant when the report stage was skipped, `keepAll:
            // true` retains the report for every build,
            // `alwaysLinkToLastBuild` makes the sidebar "Last
            // report" link point to the newest.
            //
            // publishHTML([
            //     reportDir: '.',
            //     reportFiles: 'report.html',
            //     reportName: 'perf-sentinel',
            //     keepAll: true,
            //     alwaysLinkToLastBuild: true,
            //     allowMissing: true
            // ])
        }
    }
}
