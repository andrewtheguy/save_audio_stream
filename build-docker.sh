#!/bin/bash

# Build the Docker image and extract the binary to tmp/
echo "Building save_audio_stream binary using Docker..."

# Create tmp directory if it doesn't exist
mkdir -p tmp

# Build and extract binary
docker build -f Dockerfile.build --target export --output tmp/ .

if [ $? -eq 0 ]; then
    echo "Binary built successfully and copied to tmp/save_audio_stream"
    chmod +x tmp/save_audio_stream
else
    echo "Build failed"
    exit 1
fi

# run scp -i ~/.ssh/id_rsa_oracle tmp/save_audio_stream opc@private.hpmp.net:~/ to update the server binary