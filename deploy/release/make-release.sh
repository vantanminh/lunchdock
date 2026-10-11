#!/bin/sh
# Write release.json and SHA256SUMS for a Rudder release.
#
#   deploy/release/make-release.sh VERSION DIR PANEL_IMAGE CACHE_IMAGE
#
# DIR holds the release files (binaries, install scripts) and an agents/
# directory with rudder-agent-<target>. Both images must be pinned by digest
# (name@sha256:...). Sign the result with:
#
#   minisign -S -s rudder.key -m DIR/SHA256SUMS -t "rudder VERSION"
set -eu

if [ "$#" -ne 4 ]; then
    echo "usage: $0 VERSION DIR PANEL_IMAGE CACHE_IMAGE" >&2
    exit 2
fi
version=$1
dir=$2
panel=$3
cache=$4

if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$'; then
    echo "version must look like 1.2.3 or 1.2.3-rc.1, got '$version'" >&2
    exit 2
fi
for image in "$panel" "$cache"; do
    if ! printf '%s' "$image" | grep -Eq '^[a-z0-9./_:-]+@sha256:[0-9a-f]{64}$'; then
        echo "image must be pinned by digest (name@sha256:...), got '$image'" >&2
        exit 2
    fi
done

agents=""
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
    file="$dir/agents/rudder-agent-$target"
    if [ -f "$file" ]; then
        sum=$(sha256sum "$file" | cut -d ' ' -f 1)
        cp "$file" "$dir/rudder-agent-$target"
        [ -z "$agents" ] || agents="$agents,"
        agents="$agents
    \"$target\": \"$sum\""
    fi
done
if [ -z "$agents" ]; then
    echo "no agent binaries in $dir/agents" >&2
    exit 1
fi

cat > "$dir/release.json" <<EOF
{
  "format": "rudder-release",
  "version": "$version",
  "images": {
    "panel": "$panel",
    "cache": "$cache"
  },
  "agents": {$agents
  }
}
EOF

(
    cd "$dir"
    rm -f SHA256SUMS SHA256SUMS.minisig
    find . -maxdepth 1 -type f ! -name 'SHA256SUMS*' -printf '%f\n' | LC_ALL=C sort | xargs sha256sum > SHA256SUMS
)
echo "wrote $dir/release.json and $dir/SHA256SUMS"
