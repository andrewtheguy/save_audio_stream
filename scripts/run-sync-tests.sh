#!/bin/bash

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Default PostgreSQL configuration (assumes local server already running)
DEFAULT_POSTGRES_URL="postgres://it3@localhost:5432"
DEFAULT_POSTGRES_PASSWORD="qwertasdfg"

# Use environment variables if set, otherwise use defaults
POSTGRES_URL="${TEST_POSTGRES_URL:-$DEFAULT_POSTGRES_URL}"
POSTGRES_PASSWORD="${TEST_POSTGRES_PASSWORD:-$DEFAULT_POSTGRES_PASSWORD}"

echo -e "${GREEN}Running PostgreSQL sync tests${NC}"
echo -e "PostgreSQL URL: ${YELLOW}$POSTGRES_URL${NC}"
echo ""

# Check if psql is available for connectivity test
if command -v psql &> /dev/null; then
    echo "Checking PostgreSQL connectivity..."
    if PGPASSWORD="$POSTGRES_PASSWORD" psql "$POSTGRES_URL" -c "SELECT 1" > /dev/null 2>&1; then
        echo -e "${GREEN}✓ PostgreSQL connection successful${NC}"
    else
        echo -e "${RED}✗ Cannot connect to PostgreSQL${NC}"
        echo ""
        echo "Make sure PostgreSQL is running and accessible at: $POSTGRES_URL"
        echo ""
        echo "To override connection settings:"
        echo "  TEST_POSTGRES_URL=postgres://user@host:port TEST_POSTGRES_PASSWORD=pass $0"
        exit 1
    fi
else
    echo -e "${YELLOW}psql not found, skipping connectivity check${NC}"
fi

echo ""
echo -e "${GREEN}Running sync integration tests...${NC}"
echo ""

# Run tests with environment variables
if TEST_POSTGRES_URL="$POSTGRES_URL" TEST_POSTGRES_PASSWORD="$POSTGRES_PASSWORD" cargo test --test sync_test -- --ignored; then
    echo ""
    echo -e "${GREEN}✓ All sync tests passed!${NC}"
    EXIT_CODE=0
else
    echo ""
    echo -e "${RED}✗ Some sync tests failed${NC}"
    EXIT_CODE=1
fi

exit $EXIT_CODE
