ARG BASE_IMAGE=nikolaik/python-nodejs@sha256:3a486570ce499b396f0189e27a782410801a27503b0f82b95ed51bfd01d72478
FROM ${BASE_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive \
    PLAYWRIGHT_BROWSERS_PATH=/opt/agent-tools/browsers \
    NODE_PATH=/opt/agent-tools/node_modules \
    PATH=/opt/agent-tools/python/bin:/opt/agent-tools/node_modules/.bin:${PATH}
RUN apt-get update && apt-get install -y --no-install-recommends \
      breeze-icon-theme ca-certificates curl dbus-x11 lxqt-core openbox \
      qterminal tigervnc-standalone-server fonts-noto-cjk \
      x11-utils xdotool xclip xauth scrot mousepad procps \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1001 -s /bin/bash desktop \
    && mkdir -p /opt/agent-tools /opt/image-manifest
WORKDIR /opt/agent-tools
COPY images/desktop/package.json images/desktop/package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund \
    && node node_modules/playwright/cli.js install --with-deps --no-shell chromium \
    && chmod -R a+rX /opt/agent-tools \
    && rm -rf /root/.npm /var/lib/apt/lists/*
COPY images/desktop/requirements.lock ./
RUN python3 -m venv /opt/agent-tools/python \
    && /opt/agent-tools/python/bin/pip install --no-cache-dir --require-hashes -r requirements.lock
COPY guest/desktop/ /opt/agent-space/desktop/
RUN dpkg-query -W -f='${Package}\t${Version}\n' > /opt/image-manifest/apt-packages.tsv \
    && python3 --version > /opt/image-manifest/python-version.txt \
    && node --version > /opt/image-manifest/node-version.txt \
    && cp node_modules/playwright-core/browsers.json /opt/image-manifest/playwright-browsers.json \
    && find /opt/agent-tools/browsers -type f -name chrome -exec sha256sum {} \; > /opt/image-manifest/browser-sha256.txt
LABEL io.runcove.image="workspace" \
      io.runcove.worker-contract="1" \
      io.agent-space.playwright="1.63.0"
WORKDIR /workspace
ENTRYPOINT []
CMD ["/bin/bash"]
