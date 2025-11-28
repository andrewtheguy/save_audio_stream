# Deno stage
FROM denoland/deno:2.5.6 AS deno

# Build stage
FROM rust:1.91-slim-trixie AS builder
ARG TARGETARCH

# Copy Deno from official image
COPY --from=deno /usr/bin/deno /usr/local/bin/deno

# Install build dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    cmake \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /build

# Copy source code
COPY . .

# Build frontend first (outside of cargo cache to ensure it always exists)
RUN cd frontend && deno task build

# Build the release binary with architecture-specific cache mounts
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-v2-${TARGETARCH} \
    --mount=type=cache,target=/build/target,id=cargo-target-v2-${TARGETARCH} \
    cargo build --release && \
    cp target/release/save_audio_stream /save_audio_stream

# Export stage - for extracting standalone binaries (used by docker-bake.hcl)
FROM scratch AS export
COPY --from=builder /save_audio_stream /save_audio_stream

# Runtime stage - minimal image for container deployment (builds from source)
FROM debian:trixie-slim AS runtime

# Install runtime dependencies (SSL for HTTPS streams)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /save_audio_stream /usr/local/bin/save_audio_stream

ENTRYPOINT ["/usr/local/bin/save_audio_stream"]

# Runtime stage for pre-built binary (used by CI to avoid double build)
FROM debian:trixie-slim AS runtime-prebuilt

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Binary must be passed via build context
COPY save_audio_stream /usr/local/bin/save_audio_stream

ENTRYPOINT ["/usr/local/bin/save_audio_stream"]
