#!/bin/bash

set -e

# Parse arguments
SKIP_PUSH=false
for arg in "$@"; do
    case $arg in
        --no-push)
            SKIP_PUSH=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [--no-push]"
            echo "  --no-push  Skip pushing to GitHub Container Registry"
            exit 0
            ;;
    esac
done

# Build the Docker image and extract the binary to tmp/
echo "Building save_audio_stream binaries using Docker..."

# Create tmp directory if it doesn't exist
mkdir -p tmp

# Ensure buildx is available
docker buildx create --name multiarch --use 2>/dev/null || docker buildx use multiarch

# Build both architectures in parallel using bake (extracts binaries)
docker buildx bake

if [ $? -ne 0 ]; then
    echo "Build failed"
    exit 1
fi

# Move binaries to final locations
mv tmp/amd64/save_audio_stream tmp/save_audio_stream-amd64
mv tmp/arm64/save_audio_stream tmp/save_audio_stream-arm64
rmdir tmp/amd64 tmp/arm64
chmod +x tmp/save_audio_stream-amd64 tmp/save_audio_stream-arm64

echo "Both binaries built successfully!"
echo "  tmp/save_audio_stream-amd64"
echo "  tmp/save_audio_stream-arm64"

# Push to GitHub Container Registry (unless --no-push)
if [ "$SKIP_PUSH" = false ]; then
    GHCR_IMAGE="ghcr.io/andrewtheguy/save_audio_stream"
    TAG=$(date -u +"%Y%m%d%H%M%S")

    echo ""
    echo "Building and pushing Docker image to ${GHCR_IMAGE}:${TAG}..."

    docker buildx build \
        --platform linux/amd64,linux/arm64 \
        --tag "${GHCR_IMAGE}:${TAG}" \
        --tag "${GHCR_IMAGE}:latest" \
        --push \
        .

    echo ""
    echo "Docker image pushed successfully!"
    echo "  ${GHCR_IMAGE}:${TAG}"
    echo "  ${GHCR_IMAGE}:latest"
else
    echo ""
    echo "Skipping push to GitHub Container Registry (--no-push)"
fi

# run scp -i ~/.ssh/id_rsa_oracle tmp/save_audio_stream-arm64 opc@private.hpmp.net:~/ to update the server binary
