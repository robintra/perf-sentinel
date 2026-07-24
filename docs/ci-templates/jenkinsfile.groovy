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
//   - Copy Artifact plugin     (optional, only needed if you enable the
//                               Diff-tab baseline wiring in the HTML
//                               report stage below)
//
// See docs/CI.md (English) or docs/FR/CI-FR.md (French) for the full CI
// integration guide and the quality-gate philosophy.

// Job whose last successful build supplies the Diff-tab baseline. On a PR
// build (env.CHANGE_TARGET set by MultiBranch Pipeline) that is the target
// branch's job. Outside a PR there is no target to infer here (this stage
// runs without a git checkout), so callers fall back to the current job's
// own history instead. Folder is derived from JOB_NAME, branch re-encoded
// %2F the way Jenkins names MultiBranch jobs (release/2.0 -> release%2F2.0).
def baseBranchJob() {
    if (!env.CHANGE_TARGET) {
        return ''
    }
    def jobName = env.JOB_NAME ?: ''
    def folder  = jobName.contains('/') ? jobName.substring(0, jobName.lastIndexOf('/')) : ''
    def branch  = env.CHANGE_TARGET.replace('/', '%2F')
    return folder ? "${folder}/${branch}" : branch
}

// Best-effort copy of perf-sentinel-report.json from jobName's last
// successful-or-unstable build into baseline/. Returns true if a baseline
// was found. selector: lastSuccessful(stable: false) deliberately includes
// UNSTABLE builds, not just SUCCESS ones: the Install/analyze stages above
// mark a build UNSTABLE on a tooling hiccup while still archiving a valid
// report, and the plugin's own default selector only matches SUCCESS,
// which would silently skip those and fall back further than intended. A
// missing job, missing artifact, or missing Copy Artifact plugin all
// resolve to false rather than failing the build (optional: true), since
// the Diff tab is a nice-to-have, not a build requirement.
def fetchBaseline(String jobName) {
    if (!jobName) {
        return false
    }
    copyArtifacts(
        projectName: jobName,
        filter: 'perf-sentinel-report.json',
        target: 'baseline',
        optional: true,
        fingerprintArtifacts: false,
        selector: lastSuccessful(stable: false)
    )
    return fileExists('baseline/perf-sentinel-report.json')
}

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
        PERF_SENTINEL_VERSION = '0.9.15'
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
                // Download only, wrapped in catchError: a broken download or
                // blocked egress is a tooling problem, not a performance
                // regression. It must never redden a branch build the same
                // way a real threshold breach would (the 'Quality gate (PR
                // only)' stage below is guarded to skip rather than
                // misreport a tooling failure as a breach).
                catchError(buildResult: 'UNSTABLE', stageResult: 'UNSTABLE',
                           message: 'perf-sentinel: download failed, stage marked unstable') {
                    sh '''
                        set -euo pipefail
                        BASE_URL="https://github.com/robintra/perf-sentinel/releases/download/v${PERF_SENTINEL_VERSION}"
                        curl -sSLf -o perf-sentinel-linux-amd64 "${BASE_URL}/perf-sentinel-linux-amd64"
                        curl -sSLf -o SHA256SUMS.txt            "${BASE_URL}/SHA256SUMS.txt"
                    '''
                }
                // Verify and install, NOT wrapped in catchError: a checksum
                // mismatch means a tampered or corrupted release, not a
                // transient tooling blip. It must always fail the build
                // hard, on trunk and on PR builds alike. Skipped (not
                // failed) rather than run against missing files when the
                // download above did not produce both of them.
                script {
                    if (fileExists('perf-sentinel-linux-amd64') && fileExists('SHA256SUMS.txt')) {
                        sh '''
                            set -euo pipefail
                            grep 'perf-sentinel-linux-amd64' SHA256SUMS.txt | sha256sum -c -
                            mv perf-sentinel-linux-amd64 perf-sentinel
                            chmod +x perf-sentinel
                            ./perf-sentinel --version
                        '''
                    }
                }
            }
        }

        stage('perf-sentinel analyze') {
            steps {
                // Same tooling-failure isolation as the install stage above:
                // a crash here (missing binary, malformed traces file) marks
                // the build unstable instead of failing it outright.
                catchError(buildResult: 'UNSTABLE', stageResult: 'UNSTABLE',
                           message: 'perf-sentinel: analyze failed, stage marked unstable') {
                    // SARIF/JSON artifacts for Warnings NG and downstream
                    // archival. Always produced (no --ci) so the report
                    // exists even when the gate would fail. Written to a
                    // .tmp path first and renamed only on success: shell '>'
                    // redirection creates its target file before the
                    // command even runs, so without the rename a crashed
                    // analyze would still leave an empty
                    // perf-sentinel-results.sarif behind and defeat the
                    // fileExists() guard on the 'Quality gate' stage below.
                    sh '''
                        set -euo pipefail
                        ./perf-sentinel analyze \\
                            --input ${PERF_SENTINEL_TRACES} \\
                            --config ${PERF_SENTINEL_CONFIG} \\
                            --format sarif > perf-sentinel-results.sarif.tmp
                        mv -f perf-sentinel-results.sarif.tmp perf-sentinel-results.sarif

                        ./perf-sentinel analyze \\
                            --input ${PERF_SENTINEL_TRACES} \\
                            --config ${PERF_SENTINEL_CONFIG} \\
                            --format json > perf-sentinel-report.json.tmp
                        mv -f perf-sentinel-report.json.tmp perf-sentinel-report.json
                    '''
                }
            }
        }

        // Optional: produce the interactive HTML dashboard that the
        // HTML Publisher plugin exposes under `${BUILD_URL}perf-sentinel/`.
        // Works on both branch and pull-request builds. The Diff tab is
        // populated when a baseline is found via the two helper functions
        // above (requires the Copy Artifact plugin): on a PR build it tries
        // the target branch's last successful build first, falling back to
        // this job's own last successful build otherwise. Without a
        // baseline the other tabs (Findings, Explain, pg_stat,
        // Correlations, GreenOps) still render normally.
        // See docs/CI.md "Interactive report via Jenkins HTML Publisher"
        // for the full setup.
        //
        // stage('Generate interactive HTML report') {
        //     steps {
        //         script {
        //             def baseJob = baseBranchJob()
        //             def hasBaseline = fetchBaseline(baseJob)
        //             if (!hasBaseline && env.JOB_NAME != baseJob) {
        //                 hasBaseline = fetchBaseline(env.JOB_NAME)
        //             }
        //             withEnv(["BEFORE_OPT=${hasBaseline ? '--before baseline/perf-sentinel-report.json' : ''}"]) {
        //                 sh '''
        //                     set -euo pipefail
        //                     ./perf-sentinel report \\
        //                         --input ${PERF_SENTINEL_TRACES} \\
        //                         --config ${PERF_SENTINEL_CONFIG} \\
        //                         $BEFORE_OPT \\
        //                         --output report.html
        //                 '''
        //             }
        //         }
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
            //
            // The fileExists check ensures this stage only runs a real
            // threshold check: if the earlier analyze stage never produced
            // a SARIF (install/tooling failure, already caught above and
            // left the build UNSTABLE), skipping here avoids reporting a
            // tooling problem as a false quality-gate breach on the PR.
            when {
                allOf {
                    expression { env.CHANGE_ID != null }
                    expression { fileExists('perf-sentinel-results.sarif') }
                }
            }
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
