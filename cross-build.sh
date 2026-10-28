#!/usr/bin/env bash
# Build dist/codex-launcher.exe(.sha256) in Docker. No layer cache, so every
# run is a clean build; the image tag carries the build time.
set -euo pipefail
cd "$(dirname "$0")"
tag="codex-launcher-cross:$(date +%Y%m%d-%H%M%S)"
rm -rf dist && mkdir dist
docker build --no-cache --pull -f Dockerfile.cross -t "$tag" .
id=$(docker create "$tag")
trap 'docker rm "$id" >/dev/null' EXIT
docker cp "$id:/out/." dist/
echo "built with $tag"
cat dist/codex-launcher.exe.sha256
