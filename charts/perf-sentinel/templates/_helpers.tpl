{{/*
Expand the name of the chart.
*/}}
{{- define "perf-sentinel.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Fully qualified app name. Truncated at 63 chars because some Kubernetes name
fields are limited to this (DNS-1123 label).
*/}}
{{- define "perf-sentinel.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{/*
Chart label value (<name>-<version>), sanitized for label usage.
*/}}
{{- define "perf-sentinel.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Common labels applied to every rendered object.
*/}}
{{- define "perf-sentinel.labels" -}}
helm.sh/chart: {{ include "perf-sentinel.chart" . }}
{{ include "perf-sentinel.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- with .Values.commonLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{/*
Selector labels. Stable across upgrades, never include version.
*/}}
{{- define "perf-sentinel.selectorLabels" -}}
app.kubernetes.io/name: {{ include "perf-sentinel.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{/*
ServiceAccount name, honoring .Values.serviceAccount.create.
*/}}
{{- define "perf-sentinel.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
{{- default (include "perf-sentinel.fullname" .) .Values.serviceAccount.name -}}
{{- else -}}
{{- default "default" .Values.serviceAccount.name -}}
{{- end -}}
{{- end -}}

{{/*
Resolved image reference. Falls back to .Chart.AppVersion when no tag is set.
*/}}
{{- define "perf-sentinel.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}

{{/*
Hash of the rendered ConfigMap, used as a podTemplate annotation so that
`helm upgrade` rolls the pods when .perf-sentinel.toml changes.
*/}}
{{- define "perf-sentinel.configChecksum" -}}
{{- $cm := include (print $.Template.BasePath "/configmap.yaml") . -}}
{{- $cm | sha256sum -}}
{{- end -}}

{{/*
Headless Service name used by the StatefulSet when workload.kind=StatefulSet.
Defaults to the full release name.
*/}}
{{- define "perf-sentinel.statefulset.serviceName" -}}
{{- default (include "perf-sentinel.fullname" .) .Values.workload.statefulset.serviceName -}}
{{- end -}}

{{/*
ConfigMap name. Derived from the fullname, truncated at 63 chars to stay
within the DNS-1123 label limit even for long release names.
*/}}
{{- define "perf-sentinel.configMapName" -}}
{{- printf "%s-config" (include "perf-sentinel.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
helm test Pod name. Same 63-char guard as the ConfigMap.
*/}}
{{- define "perf-sentinel.testPodName" -}}
{{- printf "%s-test-connection" (include "perf-sentinel.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Shared pod spec used by Deployment, DaemonSet and StatefulSet.
*/}}
{{- define "perf-sentinel.podSpec" -}}
serviceAccountName: {{ include "perf-sentinel.serviceAccountName" . }}
automountServiceAccountToken: {{ .Values.serviceAccount.automountServiceAccountToken }}
{{- with .Values.image.pullSecrets }}
imagePullSecrets:
  {{- toYaml . | nindent 2 }}
{{- end }}
securityContext:
  {{- toYaml .Values.podSecurityContext | nindent 2 }}
containers:
  - name: perf-sentinel
    image: {{ include "perf-sentinel.image" . | quote }}
    imagePullPolicy: {{ .Values.image.pullPolicy }}
    args:
      - watch
      - --config
      - /etc/perf-sentinel/.perf-sentinel.toml
      {{- with .Values.extraArgs }}
      {{- toYaml . | nindent 6 }}
      {{- end }}
    ports:
      - name: otlp-grpc
        containerPort: {{ .Values.service.ports.otlpGrpc.port }}
        protocol: TCP
      - name: otlp-http
        containerPort: {{ .Values.service.ports.otlpHttp.port }}
        protocol: TCP
    {{- with .Values.livenessProbe }}
    livenessProbe:
      {{- toYaml . | nindent 6 }}
    {{- end }}
    {{- with .Values.readinessProbe }}
    readinessProbe:
      {{- toYaml . | nindent 6 }}
    {{- end }}
    {{- with .Values.extraEnv }}
    env:
      {{- toYaml . | nindent 6 }}
    {{- end }}
    {{- with .Values.extraEnvFrom }}
    envFrom:
      {{- toYaml . | nindent 6 }}
    {{- end }}
    resources:
      {{- toYaml .Values.resources | nindent 6 }}
    securityContext:
      {{- toYaml .Values.securityContext | nindent 6 }}
    volumeMounts:
      - name: config
        mountPath: /etc/perf-sentinel/.perf-sentinel.toml
        subPath: perf-sentinel.toml
        readOnly: true
      - name: tmp
        mountPath: /tmp
      {{- if and (eq .Values.workload.kind "StatefulSet") .Values.workload.statefulset.persistence.enabled }}
      - name: data
        mountPath: /var/lib/perf-sentinel
      {{- end }}
      {{- with .Values.extraVolumeMounts }}
      {{- toYaml . | nindent 6 }}
      {{- end }}
volumes:
  - name: config
    configMap:
      name: {{ include "perf-sentinel.configMapName" . }}
  - name: tmp
    emptyDir: {}
  {{- with .Values.extraVolumes }}
  {{- toYaml . | nindent 2 }}
  {{- end }}
{{- with .Values.nodeSelector }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.tolerations }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.affinity }}
affinity:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.topologySpreadConstraints }}
topologySpreadConstraints:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end -}}

{{/*
Pod template annotations: always include the ConfigMap checksum so edits
trigger a rollout, then merge any user-supplied podAnnotations.
*/}}
{{- define "perf-sentinel.podAnnotations" -}}
checksum/config: {{ include "perf-sentinel.configChecksum" . }}
{{- with .Values.podAnnotations }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{/*
Pod template labels merge commonLabels / selectorLabels / podLabels.
*/}}
{{- define "perf-sentinel.podLabels" -}}
{{ include "perf-sentinel.selectorLabels" . }}
{{- with .Values.podLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}
