ARG BASE_IMAGE=nikolaik/python-nodejs@sha256:648f6a6036dca5fc9de144ca54ce7c0af51ac49af790ed67c2ec6a81cbfac3b3
FROM scratch AS chrome-artifact
ARG CHROMIUM_URL
ARG CHROMIUM_SHA256
ADD --checksum=sha256:${CHROMIUM_SHA256} ${CHROMIUM_URL} /chrome-linux64.zip

FROM ${BASE_IMAGE}
ENV DEBIAN_FRONTEND=noninteractive \
    PLAYWRIGHT_BROWSERS_PATH=/opt/agent-tools/browsers \
    NODE_PATH=/opt/agent-tools/node_modules \
    PATH=/opt/agent-tools/python/bin:/opt/agent-tools/node_modules/.bin:${PATH}
RUN apt-get update && apt-get install -y --no-install-recommends \
      breeze-icon-theme ca-certificates curl dbus-x11 lxqt-core openbox \
      qterminal tigervnc-standalone-server fonts-noto-cjk \
      x11-utils xdotool xclip xauth scrot mousepad procps \
      ffmpeg python3 python3-pil \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1001 -s /bin/bash desktop \
    && mkdir -p /opt/agent-tools /opt/image-manifest
WORKDIR /opt/agent-tools
COPY images/desktop/package.json images/desktop/package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY images/desktop/chromium-download.lock.json /opt/image-manifest/
COPY images/desktop/install_browser.py /tmp/install_browser.py
RUN --mount=type=bind,from=chrome-artifact,source=/chrome-linux64.zip,target=/tmp/chrome-linux64.zip \
    /usr/bin/python3 /tmp/install_browser.py install \
      --lock /opt/image-manifest/chromium-download.lock.json --archive /tmp/chrome-linux64.zip \
    && chmod -R a+rX /opt/agent-tools \
    && rm /tmp/install_browser.py \
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
