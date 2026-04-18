// perf-sentinel Jenkins declarative pipeline
//
// Runs perf-sentinel in batch mode against an integration-test trace fixture,
// publishes SARIF findings via the Warnings Next Generation plugin, and
// archives JSON + SARIF artifacts. The quality gate is enforced by
// `perf-sentinel analyze --ci` (non-zero exit on threshold breach) and
// duplicated in the Warnings NG `qualityGates` for double safety.
//
// What you must adapt before using this template:
//   1. PERF_SENTINEL_VERSION: pin to an exact release tag (never use
//      'latest'). Bump deliberately and review the CHANGELOG before each bump.
//   2. PERF_SENTINEL_TRACES: path to a trace file produced by your
//      integration test stage. The Java reference setup uses an OTel Java
//      Agent with the file exporter writing to target/traces.json. See
//      ENTERPRISE-JAVA-INTEGRATION-FR.md for a worked Spring Boot + Maven
//      example with the kinexoPipeline shared library.
//   3. PERF_SENTINEL_CONFIG: path to your .perf-sentinel.toml. Tune the
//      [thresholds] section to set quality-gate severity floors.
//
// Required Jenkins plugins:
//   - Warnings Next Generation (publishes SARIF as a structured issue tree)
//   - Pipeline Utility Steps   (only if you want to readJSON the report)
//
// See docs/INTEGRATION.md (English) or docs/FR/INTEGRATION-FR.md (French) for
// the full integration guide and the quality-gate philosophy.

pipeline {
    agent any

    environment {
        PERF_SENTINEL_VERSION = '0.4.4'
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

        stage('Quality gate') {
            steps {
                // Re-run with --ci to enforce thresholds. Exit code 1 fails
                // the stage; the pipeline goes red.
                sh '''
                    set -euo pipefail
                    ./perf-sentinel analyze \\
                        --ci \\
                        --input ${PERF_SENTINEL_TRACES} \\
                        --config ${PERF_SENTINEL_CONFIG} \\
                        --format json > /dev/null
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
            // Publish SARIF via Warnings NG. Duplicates the perf-sentinel
            // gate as a defense-in-depth measure: if the SARIF lists any
            // ERROR-level issue, the build is marked FAILURE even when the
            // earlier --ci stage was somehow skipped.
            recordIssues(
                tools: [
                    sarif(
                        pattern: 'perf-sentinel-results.sarif',
                        id: 'perf-sentinel',
                        name: 'perf-sentinel'
                    )
                ],
                qualityGates: [
                    [threshold: 1, type: 'TOTAL_ERROR', criticality: 'FAILURE']
                ]
            )
        }
    }
}
