#!/bin/sh
# Install the Rudder agent on a server and connect it to a panel.
#
#   curl -fsSL https://panel.example.com/install-agent.sh | sudo sh -s -- \
#     --server https://panel.example.com --token join_...
#
# Options:
#   --server URL       Panel URL (required)
#   --token TOKEN      One-time join token from the panel (required)
#   --version VERSION  Release to fetch from GitHub when the panel has no bundled binary
#   --email EMAIL      Let's Encrypt account email for this server's Traefik
#   --no-edge          Do not start the rudder-traefik and rudder-cache containers
#
# Every value is passed to programs as its own argument; nothing is evaluated.
set -eu

REPO="${RUDDER_GITHUB_REPO:-vantanminh/lunchdock}"
BIN_DIR="/usr/local/bin"
AGENT_DATA="/var/lib/rudder-agent"
NODE_DATA="/var/lib/rudder"
UNIT="/etc/systemd/system/rudder-agent.service"
TRAEFIK_IMAGE="${RUDDER_TRAEFIK_IMAGE:-traefik:v3.3}"
CACHE_IMAGE="${RUDDER_CACHE_IMAGE:-ghcr.io/vantanminh/rudder-cache:latest}"

server=""
token=""
version="latest"
email=""
edge=1

die() {
	printf 'rudder: %s\n' "$1" >&2
	exit 1
}

info() {
	printf 'rudder: %s\n' "$1"
}

while [ $# -gt 0 ]; do
	case "$1" in
	--server)
		[ $# -ge 2 ] || die "--server needs a value"
		server="$2"
		shift 2
		;;
	--token)
		[ $# -ge 2 ] || die "--token needs a value"
		token="$2"
		shift 2
		;;
	--version)
		[ $# -ge 2 ] || die "--version needs a value"
		version="$2"
		shift 2
		;;
	--email)
		[ $# -ge 2 ] || die "--email needs a value"
		email="$2"
		shift 2
		;;
	--no-edge)
		edge=0
		shift
		;;
	*)
		die "unknown option $1"
		;;
	esac
done

[ -n "$server" ] || die "--server is required"
[ -n "$token" ] || die "--token is required"
case "$server" in
http://* | https://*) ;;
*) die "--server must start with http:// or https://" ;;
esac
server="${server%/}"
case "$version" in
latest | v[0-9]*) ;;
*) die "--version must look like v1.2.3" ;;
esac
[ "$(id -u)" -eq 0 ] || die "run as root (sudo)"
command -v curl >/dev/null 2>&1 || die "curl is required"
command -v sha256sum >/dev/null 2>&1 || die "sha256sum is required"
command -v docker >/dev/null 2>&1 || die "Docker is required: https://docs.docker.com/engine/install/"
docker info >/dev/null 2>&1 || die "Docker is installed but not running"
command -v systemctl >/dev/null 2>&1 || die "systemd is required"

case "$(uname -m)" in
x86_64 | amd64) arch="x86_64" ;;
aarch64 | arm64) arch="aarch64" ;;
*) die "unsupported architecture $(uname -m)" ;;
esac
asset="rudder-agent-${arch}-unknown-linux-musl"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fetch() {
	# fetch URL FILE
	curl -fsSL --proto '=https,http' --retry 3 -o "$2" "$1"
}

verify() {
	# verify BINARY CHECKSUM_FILE
	expected="$(cut -d ' ' -f 1 <"$2")"
	actual="$(sha256sum "$1" | cut -d ' ' -f 1)"
	[ -n "$expected" ] && [ "$expected" = "$actual" ]
}

info "downloading $asset"
if fetch "$server/downloads/$asset" "$tmp/agent" && fetch "$server/downloads/$asset.sha256" "$tmp/agent.sha256"; then
	verify "$tmp/agent" "$tmp/agent.sha256" || die "checksum mismatch for the panel's agent binary"
else
	if [ "$version" = "latest" ]; then
		base="https://github.com/$REPO/releases/latest/download"
	else
		base="https://github.com/$REPO/releases/download/$version"
	fi
	fetch "$base/$asset" "$tmp/agent" || die "could not download $asset"
	fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "could not download SHA256SUMS"
	grep " $asset\$" "$tmp/SHA256SUMS" | grep -v "$asset\." >"$tmp/agent.sha256" || die "$asset is not in SHA256SUMS"
	verify "$tmp/agent" "$tmp/agent.sha256" || die "checksum mismatch for $asset"
fi
install -m 0755 "$tmp/agent" "$BIN_DIR/rudder-agent"
info "installed $("$BIN_DIR/rudder-agent" --version)"

install -d -m 0700 "$AGENT_DATA"
install -d -m 0755 "$NODE_DATA"
if [ ! -f "$AGENT_DATA/agent.json" ]; then
	"$BIN_DIR/rudder-agent" enroll --server "$server" --token "$token" --data-dir "$AGENT_DATA"
else
	info "already enrolled; keeping $AGENT_DATA (delete it to enroll again)"
fi

if [ "$edge" -eq 1 ]; then
	info "starting rudder-traefik and rudder-cache"
	docker network inspect rudder-proxy >/dev/null 2>&1 || docker network create rudder-proxy >/dev/null
	install -d -m 0755 "$NODE_DATA/traefik" "$NODE_DATA/traefik/dynamic" "$NODE_DATA/traefik/certs" "$NODE_DATA/cache"
	if [ ! -f "$NODE_DATA/traefik/acme.json" ]; then
		install -m 0600 /dev/null "$NODE_DATA/traefik/acme.json"
	fi
	{
		echo "entryPoints:"
		echo "  web:"
		echo "    address: \":80\""
		echo "  websecure:"
		echo "    address: \":443\""
		echo "providers:"
		echo "  file:"
		echo "    directory: /etc/rudder/traefik/dynamic"
		echo "    watch: true"
		echo "accessLog:"
		echo "  format: json"
		echo "log:"
		echo "  format: json"
		echo "certificatesResolvers:"
		echo "  letsencrypt:"
		echo "    acme:"
		echo "      storage: /etc/rudder/traefik/acme.json"
		echo "      httpChallenge:"
		echo "        entryPoint: web"
	} >"$NODE_DATA/traefik/traefik.yml"
	if [ -n "$email" ]; then
		printf '      email: "%s"\n' "$(printf '%s' "$email" | tr -d '"\\\n')" >>"$NODE_DATA/traefik/traefik.yml"
	fi
	[ -f "$NODE_DATA/cache/rules.json" ] || printf '{"version":0,"sites":[],"policies":[]}\n' >"$NODE_DATA/cache/rules.json"
	if ! docker inspect rudder-traefik >/dev/null 2>&1; then
		docker pull -q "$TRAEFIK_IMAGE" >/dev/null
		docker run -d --name rudder-traefik --restart unless-stopped \
			--network rudder-proxy -p 80:80 -p 443:443 \
			--oom-score-adj -900 --cpu-shares 2048 --memory-reservation 64m \
			-v "$NODE_DATA/traefik:/etc/rudder/traefik" \
			"$TRAEFIK_IMAGE" --configFile=/etc/rudder/traefik/traefik.yml >/dev/null
	fi
	if ! docker inspect rudder-cache >/dev/null 2>&1; then
		if docker image inspect "$CACHE_IMAGE" >/dev/null 2>&1 || docker pull "$CACHE_IMAGE" >/dev/null 2>&1; then
			docker run -d --name rudder-cache --restart unless-stopped \
				--network rudder-proxy -p 127.0.0.1:9090:9090 \
				-v "$NODE_DATA/cache:/var/lib/rudder/cache" \
				"$CACHE_IMAGE" --rules /var/lib/rudder/cache/rules.json \
				--listen 0.0.0.0:8080 --admin-listen 0.0.0.0:9090 >/dev/null
		else
			info "could not pull $CACHE_IMAGE; caching stays off on this server"
		fi
	fi
fi

install -d -m 0755 "$(dirname "$UNIT")"
cat >"$UNIT" <<'EOF'
[Unit]
Description=Rudder agent
After=network-online.target docker.service
Wants=network-online.target
Requires=docker.service

[Service]
ExecStart=/usr/local/bin/rudder-agent run --data-dir /var/lib/rudder-agent --node-dir /var/lib/rudder
Restart=always
RestartSec=5
Environment=RUST_LOG=info
# Keep the agent answering when apps use up the server, so the panel can still stop them.
OOMScoreAdjust=-900
CPUWeight=1000
MemoryLow=32M

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload
systemctl enable rudder-agent >/dev/null 2>&1
systemctl restart rudder-agent
info "rudder-agent is running; the server shows as online in the panel within a few seconds"
