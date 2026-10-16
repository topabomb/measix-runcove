# ---- Stage 1: Compile Rust binaries ----
FROM nikolaik/python-nodejs@sha256:3a486570ce499b396f0189e27a782410801a27503b0f82b95ed51bfd01d72478 AS desktop-ui-builder
WORKDIR /ui
COPY desktop-ui/package.json desktop-ui/package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY desktop-ui/ ./
RUN npm run build

FROM rust:1.96-slim AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl patch python3 pkg-config libcap-ng-dev libdbus-1-dev libgcc-s1 && \
    rm -rf /var/lib/apt/lists/*

# microsandbox 0.7.2 locks libsqlite3-sys 0.30.1, whose bundled SQLite is
# 3.46.0. It predates the WAL-reset corruption fix in 3.51.3. Build the
# patched amalgamation with the same options as libsqlite3-sys and link it
# statically, so the running binary cannot fall back to the older system lib.
ARG SQLITE_VERSION=3510300
ARG SQLITE_SHA256=81f5be397049b0cae1b167f2225af7646fc0f82e4a9b3c48c9ea3a533e21d77a
RUN curl --fail --location --silent --show-error \
      "https://www.sqlite.org/2026/sqlite-autoconf-${SQLITE_VERSION}.tar.gz" -o /tmp/sqlite.tar.gz && \
    echo "${SQLITE_SHA256}  /tmp/sqlite.tar.gz" | sha256sum -c - && \
    mkdir -p /opt/sqlite/lib/pkgconfig /opt/sqlite/include && \
    tar -xzf /tmp/sqlite.tar.gz -C /tmp && \
    cp "/tmp/sqlite-autoconf-${SQLITE_VERSION}/sqlite3.h" /opt/sqlite/include/ && \
    cc -O2 -fPIC -DSQLITE_CORE -DSQLITE_DEFAULT_FOREIGN_KEYS=1 \
      -DSQLITE_ENABLE_API_ARMOR -DSQLITE_ENABLE_COLUMN_METADATA \
      -DSQLITE_ENABLE_DBSTAT_VTAB -DSQLITE_ENABLE_FTS3 \
      -DSQLITE_ENABLE_FTS3_PARENTHESIS -DSQLITE_ENABLE_FTS5 \
      -DSQLITE_ENABLE_JSON1 -DSQLITE_ENABLE_LOAD_EXTENSION=1 \
      -DSQLITE_ENABLE_MEMORY_MANAGEMENT -DSQLITE_ENABLE_RTREE \
      -DSQLITE_ENABLE_STAT4 -DSQLITE_ENABLE_UNLOCK_NOTIFY \
      -DSQLITE_SOUNDEX -DSQLITE_THREADSAFE=1 \
      -DSQLITE_USE_URI -DHAVE_USLEEP=1 -DHAVE_ISNAN -DHAVE_LOCALTIME_R \
      -c "/tmp/sqlite-autoconf-${SQLITE_VERSION}/sqlite3.c" -o /tmp/sqlite3.o && \
    ar rcs /opt/sqlite/lib/libsqlite3.a /tmp/sqlite3.o && \
    printf 'prefix=/opt/sqlite\nlibdir=${prefix}/lib\nincludedir=${prefix}/include\nName: sqlite3\nDescription: pinned SQLite for Agent Space\nVersion: 3.51.3\nLibs: -L${libdir} -lsqlite3\nCflags: -I${includedir}\n' > /opt/sqlite/lib/pkgconfig/sqlite3.pc && \
    rm -rf /tmp/sqlite-autoconf-* /tmp/sqlite.tar.gz /tmp/sqlite3.o

ENV LIBSQLITE3_SYS_USE_PKG_CONFIG=1 \
    SQLITE3_LIB_DIR=/opt/sqlite/lib \
    SQLITE3_INCLUDE_DIR=/opt/sqlite/include \
    SQLITE3_STATIC=1

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY agent-space-mcp/Cargo.toml ./agent-space-mcp/
COPY src/ ./src/
COPY agent-space-mcp/src/ ./agent-space-mcp/src/
COPY static-http/ ./static-http/
COPY webdav/ ./webdav/
COPY vendor/ ./vendor/
COPY build-support/ ./build-support/
COPY guest/ ./guest/
COPY images/ ./images/
COPY desktop-helper/ ./desktop-helper/

COPY docker/apply-msb-patch.py /build/docker/
COPY docker/patches/ /build/docker/patches/
RUN python3 /build/docker/apply-msb-patch.py --vendor-root /build/vendor

# Root Cargo.toml is both [workspace] and [package] (agent-engine lib);
# --workspace ensures the library and agent-space-mcp binary are built together.
RUN --mount=type=cache,id=runcove-server-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=runcove-server-target,target=/build/target \
    CARGO_BUILD_JOBS=2 cargo build --release --workspace --locked && \
    mkdir -p /out && cp /build/target/release/agent-space-mcp /build/target/release/agent-space-static-http /build/target/release/agent-space-webdav /build/target/release/agent-space-desktop /out/
RUN ! ldd /out/agent-space-mcp | grep -q libsqlite3 && \
    grep -aq '3.51.3' /out/agent-space-mcp

# Verify the actual linked symbol, not a string also present in embedded assets.
RUN apt-get update && apt-get install -y --no-install-recommends python3 && \
    rm -rf /var/lib/apt/lists/*
COPY docker/verify-sqlite-link.py docker/sqlite-check.c /build/docker/
RUN python3 /build/docker/verify-sqlite-link.py /out/agent-space-mcp && \
    cc -O2 -I/opt/sqlite/include /build/docker/sqlite-check.c \
      /opt/sqlite/lib/libsqlite3.a -ldl -lpthread -lm -o /build/sqlite-check

# ---- Stage 2: Rebuild the pinned VM executable with audited relay and lifecycle patches ----
# SQLite defenses must be present in every writer, including `msb machine`.
FROM builder AS runtime-builder
ARG MSB_SOURCE_REVISION=60d4dc8a436fb9365491567ec21d073e924e3c6d
ARG MSB_SOURCE_SHA256=fecd277ccfddbb4a131e4c9dfe056a87a014926c78dd705776d738158c483828
RUN curl --fail --location --retry 3 --silent --show-error \
      "https://github.com/superradcompany/microsandbox/archive/${MSB_SOURCE_REVISION}.tar.gz" -o /tmp/msb-source.tar.gz && \
    echo "${MSB_SOURCE_SHA256}  /tmp/msb-source.tar.gz" | sha256sum -c - && \
    mkdir /msb-source && tar -xzf /tmp/msb-source.tar.gz -C /msb-source --strip-components=1 && \
    rm /tmp/msb-source.tar.gz
COPY docker/apply-msb-patch.py /build/docker/
COPY docker/patches/ /build/docker/patches/
RUN python3 -c 'import json,os,pathlib; m=json.loads(pathlib.Path("/build/docker/patches/manifest.json").read_text()); assert m["source_revision"]==os.environ["MSB_SOURCE_REVISION"] and m["source_archive_sha256"]==os.environ["MSB_SOURCE_SHA256"], "MSB upstream input differs from audited patch"' && \
    python3 /build/docker/apply-msb-patch.py /msb-source
RUN --mount=type=cache,id=runcove-server-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=runcove-server-target,target=/build/target \
    cd /msb-source && \
    MSB_HOME=/opt/msb-build-home CARGO_TARGET_DIR=/build/target \
    CARGO_BUILD_JOBS=2 CARGO_PROFILE_TEST_DEBUG=0 SQLITE3_NO_PKG_CONFIG=1 \
      cargo test --locked -p microsandbox-runtime --lib runner::relay && \
    MSB_HOME=/opt/msb-build-home CARGO_TARGET_DIR=/build/target \
    CARGO_BUILD_JOBS=2 CARGO_PROFILE_TEST_DEBUG=0 SQLITE3_NO_PKG_CONFIG=1 \
      cargo test --locked -p microsandbox-runtime --lib recorded_runtime_tests -- --test-threads=1 && \
    MSB_HOME=/opt/msb-build-home CARGO_TARGET_DIR=/build/target \
    CARGO_BUILD_JOBS=2 CARGO_PROFILE_TEST_DEBUG=0 SQLITE3_NO_PKG_CONFIG=1 \
      cargo test --locked -p microsandbox-runtime --lib client::maintenance -- --test-threads=1 && \
    MSB_HOME=/opt/msb-build-home CARGO_TARGET_DIR=/build/target \
    CARGO_BUILD_JOBS=2 CARGO_PROFILE_TEST_DEBUG=0 SQLITE3_NO_PKG_CONFIG=1 \
      cargo test --locked -p microsandbox --lib backend::local::sandbox -- --test-threads=1 && \
    MSB_HOME=/opt/msb-build-home CARGO_TARGET_DIR=/build/target \
    CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_STRIP=none SQLITE3_NO_PKG_CONFIG=1 \
      cargo build --release --locked -p microsandbox-cli --bin msb && \
    cp /build/target/release/msb /out/msb && \
    python3 /build/docker/verify-sqlite-link.py /out/msb && \
    ! ldd /out/msb | grep -q libsqlite3 && \
    strip /out/msb && \
    /out/msb --version | grep -Fx 'msb 0.7.2'
RUN python3 -c 'import hashlib,json,os,pathlib; p=pathlib.Path("/out/msb"); m=json.loads(pathlib.Path("/build/docker/patches/manifest.json").read_text()); pathlib.Path("/build/runtime.json").write_text(json.dumps({"microsandbox":"0.7.2","sqlite":"3.51.3","source_revision":os.environ["MSB_SOURCE_REVISION"],"source_archive_sha256":os.environ["MSB_SOURCE_SHA256"],"sdk_source_modified":True,"runtime_source_modified":True,"runtime_patch":m,"profile":"release","msb_sha256":hashlib.file_digest(p.open("rb"),"sha256").hexdigest()},indent=2)+"\n")'
RUN printf '%s  bin/msb\n' "$(sha256sum /out/msb | cut -d' ' -f1)" > /build/msb.sha256

# Export only the rebuilt runtime/checker for repairing an older application.
FROM scratch AS runtime-assets
COPY --from=runtime-builder /out/msb /msb
COPY --from=runtime-builder /build/sqlite-check /sqlite-check
COPY --from=runtime-builder /build/runtime.json /runtime.json
COPY --from=runtime-builder /build/msb.sha256 /msb.sha256

# Record the matched desktop helper, embedded workers and UI release assets.
FROM runtime-builder AS release-metadata
ARG SOURCE_REVISION=unknown
ARG SOURCE_DIGEST=unknown
COPY docker/release-metadata.py /build/docker/
COPY --from=desktop-ui-builder /ui/dist/ /build/desktop-ui-dist/
RUN python3 -c 'import hashlib,json,pathlib; root=pathlib.Path("/build"); p=root/"runtime.json"; m=json.loads(p.read_text()); m["desktop"]=json.loads((root/"images/desktop-image.lock.json").read_text()); m["desktop"]["helper_sha256"]=hashlib.file_digest(pathlib.Path("/out/agent-space-desktop").open("rb"),"sha256").hexdigest(); m["desktop"]["ui_files"]={str(f.relative_to(root/"desktop-ui-dist")):hashlib.file_digest(f.open("rb"),"sha256").hexdigest() for f in sorted((root/"desktop-ui-dist").rglob("*")) if f.is_file()}; p.write_text(json.dumps(m,indent=2)+"\n")'

RUN python3 /build/docker/release-metadata.py --target server --bins /out --ui /build/desktop-ui-dist \
    --workspace-lock /build/images/desktop-image.lock.json --source "$SOURCE_DIGEST" --revision "$SOURCE_REVISION" \
    --output /build/release.json

# ---- Stage 3: Runtime ----
# Keep the deployment base stable; the bundled msb 0.7.2 runtime supports it.
FROM ubuntu:24.04

# Version stamp for the bundled microsandbox libkrunfw. The entrypoint uses
# this to detect whether the data volume's cached libraries are stale and must
# be refreshed. Defaults to "unknown"; build.sh passes the real msb version.
ARG MSB_LIB_VERSION=unknown
ARG APP_VERSION=unversioned
ARG SOURCE_REVISION=unknown
ARG SOURCE_DIGEST=unknown
LABEL org.opencontainers.image.title="Measix Runcove Server" \
      io.runcove.target="server" \
      io.runcove.source-sha256="${SOURCE_DIGEST}" \
      org.opencontainers.image.version="${APP_VERSION}" \
      org.opencontainers.image.revision="${SOURCE_REVISION}" \
      io.agent-space.source-sha256="${SOURCE_DIGEST}" \
      io.agent-space.microsandbox="${MSB_LIB_VERSION}" \
      io.agent-space.sqlite="3.51.3" \
      io.agent-space.state-schema="3"

# libcap-ng0 + libgcc-s1: runtime deps of msb (dynamically linked)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl iproute2 iptables procps libcap-ng0 libdbus-1-3 libgcc-s1 && \
    rm -rf /var/lib/apt/lists/*

# ---- sing-box: optional transparent proxy ----
ARG SING_BOX_VERSION=1.11.0
ADD https://github.com/SagerNet/sing-box/releases/download/v${SING_BOX_VERSION}/sing-box-${SING_BOX_VERSION}-linux-amd64.tar.gz /tmp/sing-box.tar.gz
RUN tar xzf /tmp/sing-box.tar.gz -C /tmp && \
    cp /tmp/sing-box-${SING_BOX_VERSION}-linux-amd64/sing-box /usr/local/bin/sing-box && \
    rm -rf /tmp/sing-box-*

COPY --from=runtime-builder /out/msb /usr/local/bin/msb
RUN chmod +x /usr/local/bin/msb && \
    ln -s /usr/local/bin/msb /usr/local/bin/microsandbox
COPY --from=runtime-builder /build/sqlite-check /usr/local/bin/agent-space-sqlite-check
COPY --from=release-metadata /build/runtime.json /opt/agent-space/runtime.json
COPY --from=release-metadata /build/release.json /opt/agent-space/release.json
COPY --from=runtime-builder /build/msb.sha256 /opt/agent-space/msb.sha256

# libkrunfw: microsandbox KVM runtime library (msb searches $HOME/.microsandbox/lib/)
# Place in /opt/msb-libs (outside the data volume); entrypoint copies to $HOME on first start
COPY docker/.build/libs/ /opt/msb-libs/

COPY --from=builder /out/agent-space-mcp /usr/local/bin/agent-space-mcp
COPY --from=builder /out/agent-space-static-http /usr/local/bin/agent-space-static-http
COPY --from=builder /out/agent-space-webdav /usr/local/bin/agent-space-webdav
COPY --from=builder /out/agent-space-desktop /usr/local/bin/agent-space-desktop
COPY --from=desktop-ui-builder /ui/dist/ /opt/agent-space/desktop-ui/
ENV AGENT_SPACE_DESKTOP_UI=/opt/agent-space/desktop-ui

# 配置文件均在宿主机通过 -v 挂载，见启动命令
COPY docker/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Pre-bundled sandbox image (gzip-compressed) so containers start without pulling
COPY docker/.build/images/default.tar.gz /opt/msb-images/default.tar.gz
COPY docker/.build/images/desktop.tar.gz /opt/msb-images/desktop.tar.gz
COPY docker/.build/images/desktop.reference /opt/msb-images/desktop.reference
COPY images/desktop-image.lock.json /opt/agent-space/desktop-image.lock.json

ENV HOME=/var/lib/microsandbox
ENV AGENT_SPACE_STATIC_HTTP=/usr/local/bin/agent-space-static-http
ENV AGENT_SPACE_WEBDAV=/usr/local/bin/agent-space-webdav
ENV MSB_LIB_VERSION=$MSB_LIB_VERSION

VOLUME ["/var/lib/microsandbox", "/var/log/agent-space"]
EXPOSE 8932 8933

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD curl --fail --silent --show-error --max-time 5 http://127.0.0.1:8932/healthz >/dev/null || exit 1

ENTRYPOINT ["docker-entrypoint.sh"]
