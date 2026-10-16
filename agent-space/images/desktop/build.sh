#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUTPUT="${1:-$ROOT/docker/.build/images}"
mkdir -p "$OUTPUT"
INPUT="$(find "$ROOT/images/desktop" "$ROOT/guest/desktop" -maxdepth 1 -type f ! -name '*.md' -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)"
EXPECTED_ID="$(cat "$OUTPUT/workspace.image-id" 2>/dev/null || true)"
ACTUAL_ID="$(docker image inspect runcove-workspace:build --format '{{.Id}}' 2>/dev/null || true)"
if [[ -n "$EXPECTED_ID" && "$EXPECTED_ID" == "$ACTUAL_ID" && -f "$OUTPUT/desktop.tar.gz" && -f "$OUTPUT/desktop.reference" && -f "$OUTPUT/desktop.source" && -f "$OUTPUT/desktop.archive.sha256" && "$(cat "$OUTPUT/desktop.source")" == "$INPUT" && "$(sha256sum "$OUTPUT/desktop.tar.gz" | cut -d' ' -f1)" == "$(cat "$OUTPUT/desktop.archive.sha256")" && "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["reference"])' "$ROOT/images/desktop-image.lock.json")" == "$(cat "$OUTPUT/desktop.reference")" ]]; then
    echo "[desktop] using matching offline image"
    exit 0
fi
docker build --platform linux/amd64 -t runcove-workspace:build -f "$ROOT/images/desktop/Dockerfile" "$ROOT"
TEMP="$(mktemp -d "$OUTPUT/desktop-export.XXXXXX")"
trap 'rm -f "$TEMP/docker.tar" "$TEMP/desktop.tar"; rmdir "$TEMP"' EXIT
WORKSPACE_ID="$(docker image inspect runcove-workspace:build --format '{{.Id}}')"
docker save "$WORKSPACE_ID" -o "$TEMP/docker.tar"
python3 "$ROOT/images/desktop/export.py" "$TEMP/docker.tar" "$TEMP/desktop.tar" --lock "$ROOT/images/desktop-image.lock.json" > "$OUTPUT/desktop.reference"
gzip -c -6 "$TEMP/desktop.tar" > "$OUTPUT/desktop.tar.gz"
sha256sum "$OUTPUT/desktop.tar.gz" | cut -d' ' -f1 > "$OUTPUT/desktop.archive.sha256"
printf '%s\n' "$INPUT" > "$OUTPUT/desktop.source"
printf '%s\n' "$WORKSPACE_ID" > "$OUTPUT/workspace.image-id"
echo "[desktop] $(cat "$OUTPUT/desktop.reference")"
