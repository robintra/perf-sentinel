# FROM scratch: minimal attack surface (no shell, no package manager).
# HEALTHCHECK is not supported with FROM scratch (no shell to run commands).
# For Kubernetes, use an httpGet liveness probe on /health (port 4318).
# The /health endpoint is always exposed, independent of [daemon] api_enabled.
FROM scratch
ARG TARGETARCH
COPY build/linux-${TARGETARCH}/perf-sentinel /perf-sentinel
USER 65534
EXPOSE 4317 4318
ENTRYPOINT ["/perf-sentinel", "watch"]
