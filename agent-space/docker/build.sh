#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
MSB_HOME="${MSB_HOME:-$HOME/.microsandbox}"

SANDBOX_IMAGE="nikolaik/python-nodejs:python3.12-nodejs22-slim"

# Cargo.lock identifies the SDK, not the application release. Application tags
# are derived from build input content and never overwrite historical SDK tags.
MSB_VERSION="$(python3 -c 'import sys,tomllib; print(next(p["version"] for p in tomllib.load(open(sys.argv[1],"rb"))["package"] if p["name"] == "microsandbox"))' "$PROJECT_DIR/Cargo.lock")"
"$PROJECT_DIR/images/desktop/build.sh"
SOURCE_REVISION="$(git -C "$PROJECT_DIR" rev-parse HEAD)"
SOURCE_DIGEST="$(python3 "$SCRIPT_DIR/release_identity.py")"
APP_VERSION="runcove-${SOURCE_DIGEST:0:16}"
VERSION_TAG="runcove-server:${APP_VERSION}"
IMAGE_NAME="${IMAGE_NAME:-$VERSION_TAG}"
if [[ "$IMAGE_NAME" =~ ^agent-space:[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Refusing to replace a historical SDK-version tag: $IMAGE_NAME" >&2
    exit 1
fi

BUILD_DIR="$SCRIPT_DIR/.build"
LIBS_DIR="$BUILD_DIR/libs"
IMAGES_DIR="$BUILD_DIR/images"
STAMP_DIR="$BUILD_DIR/.stamp"

info() { echo "[build] $*"; }
error() { echo "[build] ERROR: $*" >&2; exit 1; }

# ---- 1. Check prerequisites ----
info "checking prerequisites..."

if ! command -v docker &>/dev/null; then
    error "docker not found"
fi

if [ ! -d "$MSB_HOME" ]; then
    error "microsandbox home not found at $MSB_HOME (set MSB_HOME env if needed)"
fi

MSB_BIN="$MSB_HOME/bin/msb"
if [ ! -x "$MSB_BIN" ]; then
    error "msb binary not found at $MSB_BIN"
fi

info "msb version: $MSB_VERSION (from $MSB_BIN)"
[ "$MSB_VERSION" = "0.7.2" ] || error "Docker VM source is pinned to 0.7.2; review and update the runtime source before changing the SDK"
ACTUAL_MSB_VERSION="$("$MSB_BIN" --version | awk '{print $2}')"
[ "$ACTUAL_MSB_VERSION" = "$MSB_VERSION" ] || error "SDK requires msb $MSB_VERSION, found $ACTUAL_MSB_VERSION; run cargo check --locked first"
(cd "$MSB_HOME" && sha256sum -c "$PROJECT_DIR/msb-runtime.sha256") || error "runtime does not match the verified release artifacts"
docker info >/dev/null 2>&1 || error "Docker engine is unavailable; enable Docker Desktop WSL integration or start the Linux Docker daemon"

# ---- 2. Initialize configs from examples ----
CONFIGS_DIR="$SCRIPT_DIR/configs"
EXAMPLE_DIR="$SCRIPT_DIR/configs.example"
if [ ! -d "$CONFIGS_DIR" ] || [ -z "$(ls -A "$CONFIGS_DIR" 2>/dev/null)" ]; then
    info "initializing configs/ from configs.example/ ..."
    mkdir -p "$CONFIGS_DIR"
    cp -n "$EXAMPLE_DIR"/config.toml "$CONFIGS_DIR/" 2>/dev/null || true
fi

# ---- 3. Prepare build directory + version stamps ----
info "preparing build directory..."
mkdir -p "$LIBS_DIR" "$IMAGES_DIR" "$STAMP_DIR"

# Record which version each artifact was produced by. If the stamp does not
# match the current msb version, the cached artifact is stale and is refreshed.
# Version and checksum checks above prevent mixed SDK/runtime artifacts.

refresh_stamp() {
    echo "$MSB_VERSION" > "$STAMP_DIR/$1"
}

is_stamp_fresh() {
    [ -f "$STAMP_DIR/$1" ] && [ "$(cat "$STAMP_DIR/$1")" = "$MSB_VERSION" ]
}

# The verified upstream msb is a seed used for image export only. The Dockerfile
# rebuilds the actual VM executable from pinned source with the checked relay
# patch in docker/patches and SQLite
# 3.51.3; copying this prebuilt seed into the final image would undo the fix.

# ---- 5. Copy libkrunfw libraries ----
info "copying libkrunfw libraries..."
# Clear stale libs first so a version change never leaves mismatched sonames.
rm -f "$LIBS_DIR"/libkrunfw.so*
KRUN_REAL="$(find "$MSB_HOME/lib" -maxdepth 1 -type f -name 'libkrunfw.so.*' -printf '%p\n' | sort -V | tail -1)"
[ -n "$KRUN_REAL" ] || error "versioned libkrunfw not found under $MSB_HOME/lib"
KRUN_FILE="$(basename "$KRUN_REAL")"
KRUN_MAJOR="$(printf '%s\n' "$KRUN_FILE" | sed -E 's/^(libkrunfw\.so\.[0-9]+).*/\1/')"
cp "$KRUN_REAL" "$LIBS_DIR/$KRUN_FILE"
ln -s "$KRUN_FILE" "$LIBS_DIR/$KRUN_MAJOR"
ln -s "$KRUN_MAJOR" "$LIBS_DIR/libkrunfw.so"
refresh_stamp libs

# ---- 6. Export sandbox image ----
TARBALL="$IMAGES_DIR/default.tar.gz"
if [ -f "$TARBALL" ] && is_stamp_fresh image; then
    info "sandbox image already cached for $MSB_VERSION, skipping export"
else
    info "exporting sandbox image for $MSB_VERSION (first time / version changed, may take a moment)..."
    rm -f "$TARBALL"
    TMP_TAR="/tmp/agent-space-sandbox-$$.tar"
    "$MSB_BIN" image save "$SANDBOX_IMAGE" -o "$TMP_TAR" -q
    gzip -6 "$TMP_TAR"
    mv "${TMP_TAR}.gz" "$TARBALL"
    refresh_stamp image
    info "sandbox image exported"
fi

# ---- 7. Build Docker image ----
info "building docker image: $IMAGE_NAME (also tagged $VERSION_TAG) ..."
docker build \
    --build-arg MSB_LIB_VERSION="$MSB_VERSION" \
    --build-arg APP_VERSION="$APP_VERSION" \
    --build-arg SOURCE_REVISION="$SOURCE_REVISION" \
    --build-arg SOURCE_DIGEST="$SOURCE_DIGEST" \
    -f "$SCRIPT_DIR/Dockerfile" \
    -t "$IMAGE_NAME" \
    "$PROJECT_DIR"
docker tag "$IMAGE_NAME" "$VERSION_TAG"

# ---- 8. Summary ----
info "done!"
info ""
info "  Tags:   $IMAGE_NAME  and  $VERSION_TAG"
info "  Size:   $(docker image inspect "$IMAGE_NAME" --format='{{.Size}}' | numfmt --to=iec 2>/dev/null || docker image inspect "$IMAGE_NAME" --format='{{.Size}}')"
info ""
info "  Initialize the persistent user state once, then run:"
info "    export AGENT_SPACE_ADMIN_TOKEN='replace-with-a-long-random-secret'"
info "    docker run --rm -e AGENT_SPACE_ADMIN_TOKEN \\"
info "      -v agent-space-data:/var/lib/microsandbox \\"
info "      -v $SCRIPT_DIR/configs/config.toml:/etc/agent-space/config.toml:ro $VERSION_TAG init-state"
info "    docker run -d --name agent-space --stop-timeout 60 --device /dev/kvm -p 0.0.0.0:8932:8932 -p 0.0.0.0:8933:8933 \\"
info "      -e AGENT_SPACE_ADMIN_TOKEN -v agent-space-data:/var/lib/microsandbox \\"
info "      -v $SCRIPT_DIR/configs/config.toml:/etc/agent-space/config.toml:ro $VERSION_TAG"
info ""
info "  Run (with sing-box proxy, enter container to create config):"
info "    docker run -d --name agent-space --stop-timeout 60 --device /dev/kvm --device /dev/net/tun \\"
info "      --cap-add NET_ADMIN -p 0.0.0.0:8932:8932 -p 0.0.0.0:8933:8933 \\"
info "      -e AGENT_SPACE_ADMIN_TOKEN -v agent-space-data:/var/lib/microsandbox \\"
info "      -v $SCRIPT_DIR/configs/config.toml:/etc/agent-space/config.toml:ro $VERSION_TAG"
info ""
info "  Export (versioned):"
info "    docker save $VERSION_TAG | gzip > agent-space-${APP_VERSION}.tar.gz"
