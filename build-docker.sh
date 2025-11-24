#!/bin/bash

# Build the Docker image and extract the binary to tmp/
echo "Building save_audio_stream binaries using Docker..."

# Create tmp directory if it doesn't exist
mkdir -p tmp

# Ensure buildx is available
docker buildx create --name multiarch --use 2>/dev/null || docker buildx use multiarch

# Build both architectures in parallel using bake
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

# run scp -i ~/.ssh/id_rsa_oracle tmp/save_audio_stream-arm64 opc@private.hpmp.net:~/ to update the server binary
