# PMP Production Dockerfile
#
# Multi-stage build:
#   1. builder — compile the server binary with pinned toolchain
#   2. runtime — minimal runtime image with non‑root user
#
# Usage:
#   docker build -t phira-mp-plus-server .
#   docker run -v ./server_config.yml:/etc/pmp/server_config.yml:ro \
#              -v ./data:/var/lib/pmp/data \
#              -v ./plugins:/var/lib/pmp/plugins \
#              phira-mp-plus-server

# ── Stage 1: Builder ─────────────────────────────────────────────────────────
FROM rust:1.96.0-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libc-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Pre-fetch dependencies for better layer caching.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY phira-mp-plus-server/Cargo.toml phira-mp-plus-server/
COPY phira-mp-plus-server-api/Cargo.toml phira-mp-plus-server-api/
COPY phira-plugin-sdk/Cargo.toml phira-plugin-sdk/
COPY phira-mp/phira-mp-common/Cargo.toml phira-mp/phira-mp-common/
COPY phira-mp/phira-mp-macros/Cargo.toml phira-mp/phira-mp-macros/

RUN mkdir -p phira-mp-plus-server/src && echo "fn main() {}" > phira-mp-plus-server/src/main.rs
RUN mkdir -p phira-mp-plus-server-api/src && echo "// stub" > phira-mp-plus-server-api/src/lib.rs
RUN mkdir -p phira-plugin-sdk/src && echo "// stub" > phira-plugin-sdk/src/lib.rs
RUN mkdir -p phira-mp/phira-mp-common/src && echo "// stub" > phira-mp/phira-mp-common/src/lib.rs
RUN mkdir -p phira-mp/phira-mp-macros/src && echo "// stub" > phira-mp/phira-mp-macros/src/lib.rs

RUN cargo build --locked --release --workspace 2>/dev/null || true

# Now copy the real source and build the actual binary.
COPY . .

RUN cargo build --locked --release --workspace && \
    cp target/release/phira-mp-plus-server /build/pmp-server && \
    # Strip debug symbols for smaller image
    strip /build/pmp-server

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libc6 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user and required directories.
RUN groupadd --system pmp && \
    useradd --system --gid pmp --no-create-home --home-dir /var/lib/pmp pmp && \
    mkdir -p /var/lib/pmp/data /var/lib/pmp/plugins /etc/pmp && \
    chown -R pmp:pmp /var/lib/pmp

COPY --from=builder /build/pmp-server /usr/local/bin/phira-mp-plus-server

# Volumes for persistent data.
VOLUME ["/var/lib/pmp/data", "/var/lib/pmp/plugins", "/etc/pmp"]

# TCP game port + HTTP internal port
EXPOSE 12346 12347

USER pmp
WORKDIR /var/lib/pmp

# Health check: the /health/live endpoint responds 200 when the process is alive.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/phira-mp-plus-server", "--version"]

ENTRYPOINT ["/usr/local/bin/phira-mp-plus-server"]
CMD ["--config", "/etc/pmp/server_config.yml", "--no-cli"]
