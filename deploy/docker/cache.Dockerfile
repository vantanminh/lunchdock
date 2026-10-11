# rudder-cache image, built from the static release binary dist/rudder-cache-<arch>.
#
# docker buildx build -f deploy/docker/cache.Dockerfile --platform linux/amd64,linux/arm64 .
FROM scratch

ARG TARGETARCH
COPY dist/rudder-cache-${TARGETARCH} /rudder-cache

EXPOSE 8080 9090
ENTRYPOINT ["/rudder-cache"]
CMD ["--rules", "/var/lib/rudder/cache/rules.json", "--listen", "0.0.0.0:8080", "--admin-listen", "0.0.0.0:9090"]
