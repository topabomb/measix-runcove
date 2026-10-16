#!/bin/bash
set -euo pipefail
# Older entrypoints may keep a same-version cached binary. Replace it before
# delegation, preserving the application's original command and configuration.
runtime_home="${MSB_HOME:-${HOME}/.microsandbox}"
mkdir -p "$runtime_home/bin"
cp /usr/local/bin/msb "$runtime_home/bin/msb.tmp"
chmod +x "$runtime_home/bin/msb.tmp"
mv -f "$runtime_home/bin/msb.tmp" "$runtime_home/bin/msb"
(cd "$runtime_home" && sha256sum -c /opt/agent-space/msb.sha256)
exec /usr/local/bin/docker-entrypoint.sh "$@"
