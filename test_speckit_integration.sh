#!/bin/bash
# Integration test for `joey speckit` command

set -e

echo "=== Testing joey speckit integration ==="
echo ""

# Test 1: Check that the command exists
echo "Test 1: Checking that 'joey speckit' command exists..."
if ./target/debug/joey speckit --help > /dev/null 2>&1; then
    echo "✓ Command exists and shows help"
else
    echo "✗ Command failed"
    exit 1
fi
echo ""

# Test 2: Check that the command is in the main help
echo "Test 2: Checking that 'joey speckit' is in main help..."
if ./target/debug/joey --help | grep -q "speckit"; then
    echo "✓ Command is listed in main help"
else
    echo "✗ Command not found in main help"
    exit 1
fi
echo ""

# Test 3: Verify backend binary exists
echo "Test 3: Checking backend binary..."
if [ -f "./target/debug/joey-speckit-ui" ]; then
    echo "✓ Backend binary exists"
else
    echo "⚠ Backend binary not found (will use cargo run)"
fi
echo ""

# Test 4: Verify frontend directory exists
echo "Test 4: Checking frontend directory..."
if [ -d "./web/speckit-ui" ]; then
    echo "✓ Frontend directory exists"
else
    echo "✗ Frontend directory not found"
    exit 1
fi
echo ""

# Test 5: Verify frontend dependencies
echo "Test 5: Checking frontend dependencies..."
if [ -d "./web/speckit-ui/node_modules" ]; then
    echo "✓ Frontend dependencies installed"
else
    echo "⚠ Frontend dependencies not installed (run: cd web/speckit-ui && npm install)"
fi
echo ""

# Test 6: Verify package.json exists
echo "Test 6: Checking package.json..."
if [ -f "./web/speckit-ui/package.json" ]; then
    echo "✓ package.json exists"
else
    echo "✗ package.json not found"
    exit 1
fi
echo ""

echo "=== All basic integration tests passed! ==="
echo ""
echo "To run the full UI:"
echo "  ./target/debug/joey speckit"
echo ""
echo "To run with auto-open browser:"
echo "  ./target/debug/joey speckit --open"
