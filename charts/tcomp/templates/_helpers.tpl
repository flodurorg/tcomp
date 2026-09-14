{{- define "tcomp.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "tcomp.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "tcomp.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "tcomp.labels" -}}
helm.sh/chart: {{ include "tcomp.chart" . }}
{{ include "tcomp.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "tcomp.selectorLabels" -}}
app.kubernetes.io/name: {{ include "tcomp.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "tcomp.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "tcomp.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "tcomp.secretName" -}}
{{- if .Values.auth.existingSecret }}
{{- .Values.auth.existingSecret }}
{{- else }}
{{- include "tcomp.fullname" . }}
{{- end }}
{{- end }}

{{- define "tcomp.secretKey" -}}
{{- if .Values.auth.existingSecret }}
{{- .Values.auth.existingSecretKey }}
{{- else }}
{{- "token" }}
{{- end }}
{{- end }}

{{- define "tcomp.publicUrl" -}}
{{- if .Values.publicUrl }}
{{- .Values.publicUrl | trimSuffix "/" }}
{{- else if and .Values.ingress.enabled .Values.ingress.hosts }}
{{- $host := (first .Values.ingress.hosts).host }}
{{- $scheme := ternary "https" "http" (not (empty .Values.ingress.tls)) }}
{{- printf "%s://%s" $scheme $host }}
{{- else if and .Values.httpRoute.enabled .Values.httpRoute.hostnames }}
{{- printf "%s://%s" .Values.httpRoute.scheme (first .Values.httpRoute.hostnames) }}
{{- end }}
{{- end }}
