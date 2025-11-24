#!/bin/bash

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check if rclone is installed
if ! command -v rclone &> /dev/null; then
    echo -e "${RED}Error: rclone is not installed${NC}"
    echo "Please install rclone:"
    echo "  macOS:        brew install rclone"
    echo "  Ubuntu/Debian: sudo apt install rclone"
    echo "  Or download from: https://rclone.org/downloads/"
    exit 1
fi

PORT=13222

echo -e "${GREEN}Starting rclone SFTP server on port $PORT...${NC}"

# Start rclone in background
rclone serve sftp :memory: --addr ":$PORT" --user demo --pass demo > /dev/null 2>&1 &
RCLONE_PID=$!

# Ensure rclone is killed on script exit
trap "echo -e '\n${YELLOW}Stopping rclone server...${NC}'; kill $RCLONE_PID 2>/dev/null || true; wait $RCLONE_PID 2>/dev/null || true" EXIT

# Wait for rclone to be ready (check if port is listening)
echo "Waiting for rclone server to be ready..."
RETRIES=30
READY=false
for i in $(seq 1 $RETRIES); do
    if lsof -i:$PORT > /dev/null 2>&1; then
        READY=true
        break
    fi
    sleep 0.1
done

if [ "$READY" = false ]; then
    echo -e "${RED}Error: rclone server failed to start${NC}"
    exit 1
fi

echo -e "${GREEN}rclone server ready on port $PORT${NC}"
echo ""

# Run the SFTP tests
echo -e "${GREEN}Running SFTP integration tests...${NC}"
echo ""

# Run tests
if cargo test --test sftp_test -- --ignored; then
    echo ""
    echo -e "${GREEN}✓ All SFTP tests passed!${NC}"
    EXIT_CODE=0
else
    echo ""
    echo -e "${RED}✗ Some SFTP tests failed${NC}"
    EXIT_CODE=1
fi

# List all files in SFTP server before stopping
echo ""
echo -e "${YELLOW}Listing all files in SFTP server:${NC}"
rclone ls :sftp: \
    --sftp-host=localhost \
    --sftp-port=$PORT \
    --sftp-user=demo \
    --sftp-pass=$(rclone obscure demo) 2>/dev/null || echo -e "${YELLOW}(No files or listing failed)${NC}"

exit $EXIT_CODE
