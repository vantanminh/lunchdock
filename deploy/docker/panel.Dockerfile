# Rudder panel image, built from release binaries.
#
#   dist/rudder-server-<arch>                     static control-plane binary (web UI embedded)
#   dist/agents/rudder-agent-<triple>[.sha256]    agent binaries served at /downloads
#
# docker buildx build -f deploy/docker/panel.Dockerfile --platform linux/amd64,linux/arm64 .
FROM alpine:3.22

RUN apk add --no-cache \
        ca-certificates tzdata \
        git openssh-client \
        docker-cli docker-cli-buildx docker-cli-compose \
        postgresql16-client

ARG TARGETARCH
COPY dist/rudder-server-${TARGETARCH} /usr/local/bin/rudder-server
COPY dist/agents/ /usr/share/rudder/dist/

ENV RUDDER_BIND=0.0.0.0:3000 \
    RUDDER_DATA_DIR=/var/lib/rudder \
    RUDDER_DIST_DIR=/usr/share/rudder/dist \
    RUDDER_HOST_PROC=/host/proc

EXPOSE 3000 7443
ENTRYPOINT ["/usr/local/bin/rudder-server"]
