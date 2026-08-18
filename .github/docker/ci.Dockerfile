# The shared CI image: every toolchain and system dependency the check jobs
# need, baked once. Jobs start warm — no apt, no rustup download, no node
# setup, and none of the network stalls those steps are prone to.
#
# Rebuilt only when this file changes (see ci-image.yml). NOT used by the
# release build, which keeps its own clean ubuntu:22.04 (glibc baseline) path.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:/usr/local/node/bin:$PATH

RUN apt-get update && apt-get install -y --no-install-recommends \
        # Tauri build stack
        libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
        libayatana-appindicator3-dev libgtk-layer-shell-dev \
        libssl-dev libdbus-1-dev pkg-config \
        # base tooling
        build-essential curl git ca-certificates xz-utils zstd \
    && rm -rf /var/lib/apt/lists/*

# Rust stable + the components CI uses. Toolchain pinned by rustup's stable
# channel at image build time; the image rebuild cadence is the update cadence.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --component clippy rustfmt \
    && rustup --version && cargo --version

# Node 22 + pnpm 10 (the versions ci.yml sets up today).
RUN curl -fsSL https://nodejs.org/dist/v22.13.0/node-v22.13.0-linux-x64.tar.xz \
        | tar -xJ -C /usr/local --strip-components=1 \
    && npm install -g pnpm@10 \
    && node --version && pnpm --version

# cargo-about as a prebuilt binary for the licences job.
RUN curl -fsSL https://github.com/EmbarkStudios/cargo-about/releases/download/0.8.2/cargo-about-0.8.2-x86_64-unknown-linux-musl.tar.gz \
        | tar -xz --strip-components=1 -C /usr/local/cargo/bin --wildcards '*/cargo-about' \
    && cargo about --version
