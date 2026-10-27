# desktop: a Linux desktop session for proving Desktop's AppImage self-update (scripts/test-linux-update.sh):
# Ubuntu 24.04 (no libfuse2, as shipped), a virtual display, D-Bus and an unlocked keyring
# (scripts/linux-desktop-session.sh), the libraries Electron needs, and Node 22 to drive the app with Playwright.
# Runs as an ordinary user, as a desktop login would.
# pwsh: the same plus PowerShell 7, of this machine's own architecture, for scripts/test-windows-update-helper.py.
FROM ubuntu:24.04 AS desktop
ARG NODE_VERSION=22.22.0
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      ca-certificates curl xz-utils python3 procps file \
      xvfb xauth dbus dbus-x11 gnome-keyring libsecret-1-0 \
      libnss3 libgtk-3-0t64 libgbm1 libasound2t64 libxss1 libxtst6 libatk-bridge2.0-0t64 libdrm2 libxkbcommon0 \
      libxcomposite1 libxdamage1 libxrandr2 libcups2t64 libpango-1.0-0 libcairo2 libnotify4 xdg-utils git \
    && rm -rf /var/lib/apt/lists/*
RUN arch=$(dpkg --print-architecture | sed 's/amd64/x64/') \
    && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${arch}.tar.xz" \
       | tar -xJ -C /usr/local --strip-components=1
RUN useradd -m -s /bin/bash tester
USER tester
WORKDIR /home/tester

FROM desktop AS pwsh
USER root
ARG PWSH=7.4.6
RUN arch=$(dpkg --print-architecture | sed 's/amd64/x64/') \
    && curl -fsSL "https://github.com/PowerShell/PowerShell/releases/download/v${PWSH}/powershell-${PWSH}-linux-${arch}.tar.gz" -o /tmp/pwsh.tgz \
    && mkdir -p /opt/powershell && tar -xzf /tmp/pwsh.tgz -C /opt/powershell && rm /tmp/pwsh.tgz \
    && chmod +x /opt/powershell/pwsh && ln -s /opt/powershell/pwsh /usr/local/bin/pwsh
ENV DOTNET_SYSTEM_GLOBALIZATION_INVARIANT=1
USER tester
