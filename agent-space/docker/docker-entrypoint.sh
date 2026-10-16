#!/bin/bash
set -e

if [ "$#" -gt 0 ]; then
    case "$1" in
        init-state|import-users|migrate-state|convert-config)
            exec agent-space-mcp --config /etc/agent-space/config.toml "$@"
            ;;
        *)
            echo "[agent-space] ERROR: unsupported container command: $1" >&2
            exit 2
            ;;
    esac
fi

echo "[agent-space] starting..."

# 1. Check KVM (msb sandbox engine requires hardware virtualization)
if [ ! -e /dev/kvm ]; then
    echo "[agent-space] ERROR: /dev/kvm not found."
    echo "[agent-space] microsandbox requires KVM hardware virtualization."
    echo "[agent-space] Fix: docker run with --device /dev/kvm"
    exit 1
fi

# 2. Optional: start sing-box transparent proxy
SING_BOX_CONFIG="/etc/agent-space/sing-box/config.json"
if [ -f "$SING_BOX_CONFIG" ]; then
    if [ ! -e /dev/net/tun ]; then
        echo "[agent-space] WARNING: /dev/net/tun not found, sing-box TUN mode unavailable"
        echo "[agent-space]   Fix: docker run with --device /dev/net/tun"
    elif [ ! -x /usr/local/bin/sing-box ]; then
        echo "[agent-space] WARNING: sing-box binary not found"
    else
        echo "[agent-space] sing-box config found, starting..."
        # Redirect stdout/stderr to log file for debugging
        /usr/local/bin/sing-box run -c "$SING_BOX_CONFIG" \
            > /tmp/sing-box.log 2>&1 &
        SING_BOX_PID=$!
        # Brief wait to catch immediate startup failures
        sleep 1
        if kill -0 "$SING_BOX_PID" 2>/dev/null; then
            echo "[agent-space] sing-box started (PID: $SING_BOX_PID)"
        else
            echo "[agent-space] WARNING: sing-box failed to start, check config"
        fi
    fi
else
    echo "[agent-space] sing-box config not found, skipping proxy"
fi

# 3. Set msb data directory
export HOME=/var/lib/microsandbox
mkdir -p "$HOME/.microsandbox/bin" "$HOME/.microsandbox/lib"

# 4. Sync the complete msb runtime into the data volume.
#    The volume mount hides the /opt copy, so we seed it on every start.
#    We stamp the seeded version in the volume and replace every runtime file
#    on each container start. This also repairs a missing or damaged library
#    when the version stamp itself still matches.
#    microsandbox 0.7.x validates this complete installation before VM launch.
#    Fallback: if MSB_LIB_VERSION is unset/empty (image built without it), we
#    stamp by the real bundled library name so upgrades are still detected.
MSB_DATA_BIN="$HOME/.microsandbox/bin/msb"
LIB_DIR="$HOME/.microsandbox/lib"
STAMP_FILE="$LIB_DIR/.libkrunfw.version"
# Pick the highest bundled versioned libkrunfw as a natural version stamp when
# MSB_LIB_VERSION is unset. Multiple matches are handled
# by taking the max, avoiding a `basename: extra operand` error.
BUNDLED_LIB="$(find /opt/msb-libs -maxdepth 1 -type f -name 'libkrunfw.so.*' -printf '%f\n' 2>/dev/null | sort -V | tail -1)"
if [ -z "$BUNDLED_LIB" ]; then
    echo "[agent-space] ERROR: bundled libkrunfw runtime is missing"
    exit 1
fi
if [ -z "${MSB_LIB_VERSION:-}" ] || [ "$MSB_LIB_VERSION" = "unknown" ]; then
    STAMP_VAL="$BUNDLED_LIB"
else
    STAMP_VAL="$MSB_LIB_VERSION"
fi
echo "[agent-space] synchronizing msb runtime (stamp: $(cat "$STAMP_FILE" 2>/dev/null || echo none) -> $STAMP_VAL)..."
cp /usr/local/bin/msb "$MSB_DATA_BIN.tmp"
chmod +x "$MSB_DATA_BIN.tmp"
mv -f "$MSB_DATA_BIN.tmp" "$MSB_DATA_BIN"
(cd "$HOME/.microsandbox" && sha256sum -c /opt/agent-space/msb.sha256)
rm -f "$LIB_DIR"/libkrunfw.so*
cp -P /opt/msb-libs/libkrunfw.so* "$LIB_DIR/"
printf '%s\n' "$STAMP_VAL" > "$STAMP_FILE.tmp"
mv -f "$STAMP_FILE.tmp" "$STAMP_FILE"

# 5. Pre-load bundled sandbox image if not already cached
DEFAULT_IMAGE="nikolaik/python-nodejs:python3.12-nodejs22-slim"
if ! msb image list 2>/dev/null | grep -q "$DEFAULT_IMAGE"; then
    echo "[agent-space] loading bundled sandbox image (first start, ~30s)..."
    # msb image load requires -i file (stdin pipe is unreliable); decompress first
    gunzip -c /opt/msb-images/default.tar.gz > /tmp/default.tar
    if msb image load -i /tmp/default.tar -q 2>/dev/null; then
        echo "[agent-space] sandbox image loaded"
    else
        echo "[agent-space] WARNING: failed to load bundled image, will pull on first sandbox"
    fi
    rm -f /tmp/default.tar
else
    echo "[agent-space] sandbox image already cached"
fi

# The desktop preset is an offline release asset, qualified with MSB's exact
# manifest reference. Do not pull a guessed registry tag or silently substitute.
DESKTOP_IMAGE="$(cat /opt/msb-images/desktop.reference)"
if ! msb image inspect "$DESKTOP_IMAGE" >/dev/null 2>&1; then
    echo "[agent-space] loading bundled desktop image..."
    gunzip -c /opt/msb-images/desktop.tar.gz > /tmp/agent-space-desktop.oci.tar
    msb image load -i /tmp/agent-space-desktop.oci.tar -q
    rm -f /tmp/agent-space-desktop.oci.tar
fi
msb image inspect "$DESKTOP_IMAGE" >/dev/null

# 6. Start agent-space-mcp (foreground, takes over PID 1)
echo "[agent-space] starting agent-space-mcp on port 8932..."
exec agent-space-mcp --config /etc/agent-space/config.toml
