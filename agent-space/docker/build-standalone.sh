#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
"$ROOT/images/desktop/build.sh"
WORKSPACE_ID="$(cat "$ROOT/docker/.build/images/workspace.image-id")"
# A content-named local tag works with both Docker image stores and requires no
# registry push. Refuse to overwrite a different image at this content identity.
WORKSPACE_IMAGE="runcove-workspace:sha256-${WORKSPACE_ID#sha256:}"
EXISTING_ID="$(docker image inspect "$WORKSPACE_IMAGE" --format '{{.Id}}' 2>/dev/null || true)"
[ -z "$EXISTING_ID" ] || [ "$EXISTING_ID" = "$WORKSPACE_ID" ] || { echo "Workspace content tag conflict" >&2; exit 1; }
docker tag "$WORKSPACE_ID" "$WORKSPACE_IMAGE"
SOURCE_REVISION="$(git -C "$ROOT" rev-parse HEAD)"
SOURCE_DIGEST="$(python3 "$ROOT/docker/release_identity.py")"
APP_VERSION="runcove-${SOURCE_DIGEST:0:16}"
IMAGE_NAME="${IMAGE_NAME:-runcove-standalone:$APP_VERSION}"
docker build --platform linux/amd64 -f "$ROOT/docker/Dockerfile.standalone" \
  --build-arg WORKSPACE_IMAGE="$WORKSPACE_IMAGE" --build-arg SOURCE_REVISION="$SOURCE_REVISION" --build-arg SOURCE_DIGEST="$SOURCE_DIGEST" \
  --build-arg APP_VERSION="$APP_VERSION" -t "$IMAGE_NAME" "$ROOT"
[ "$(docker image inspect "$WORKSPACE_IMAGE" --format '{{.Id}}')" = "$WORKSPACE_ID" ] || { echo "Workspace identity changed during build" >&2; exit 1; }
printf 'Built %s (%s)\n' "$IMAGE_NAME" "$(docker image inspect "$IMAGE_NAME" --format '{{.Id}}')"
