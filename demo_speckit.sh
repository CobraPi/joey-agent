#!/bin/bash
# Demo script for `joey speckit` command

echo "=========================================="
echo "  Joey SpecKit UI Launcher Demo"
echo "=========================================="
echo ""

# Show the help
echo "1. Command help:"
echo "   $ joey speckit --help"
echo ""
./target/debug/joey speckit --help
echo ""
echo "------------------------------------------"
echo ""

# Show it's in the main help
echo "2. Listed in main help:"
echo "   $ joey --help | grep speckit"
echo ""
./target/debug/joey --help | grep -A1 "speckit"
echo ""
echo "------------------------------------------"
echo ""

# Show example commands
echo "3. Example usage:"
echo ""
echo "   Start with defaults:"
echo "   $ joey speckit"
echo ""
echo "   Start on custom port:"
echo "   $ joey speckit --port 3000"
echo ""
echo "   Start and open browser:"
echo "   $ joey speckit --open"
echo ""
echo "   Start with custom repo root:"
echo "   $ joey speckit --repo-root /path/to/repo"
echo ""
echo "------------------------------------------"
echo ""

# Show what will happen
echo "4. What happens when you run 'joey speckit':"
echo ""
echo "   ✓ Spawns backend (joey-speckit-ui) on port 4173"
echo "   ✓ Spawns frontend (npm run dev) in web/speckit-ui"
echo "   ✓ Manages both processes concurrently"
echo "   ✓ Press Ctrl+C to shut down both servers"
echo ""
echo "------------------------------------------"
echo ""

# Verify prerequisites
echo "5. Prerequisites check:"
echo ""

# Check backend
if [ -f "./target/debug/joey-speckit-ui" ]; then
    echo "   ✓ Backend binary found at ./target/debug/joey-speckit-ui"
else
    echo "   ⚠ Backend binary not found (will use cargo run)"
fi

# Check frontend dir
if [ -d "./web/speckit-ui" ]; then
    echo "   ✓ Frontend directory exists at ./web/speckit-ui"
else
    echo "   ✗ Frontend directory not found"
fi

# Check npm
if command -v npm &> /dev/null; then
    echo "   ✓ npm is available: $(npm --version)"
else
    echo "   ✗ npm not found"
fi

# Check node
if command -v node &> /dev/null; then
    echo "   ✓ node is available: $(node --version)"
else
    echo "   ✗ node not found"
fi

# Check frontend deps
if [ -d "./web/speckit-ui/node_modules" ]; then
    echo "   ✓ Frontend dependencies installed"
else
    echo "   ⚠ Frontend dependencies not installed"
    echo "     Run: cd web/speckit-ui && npm install"
fi

echo ""
echo "------------------------------------------"
echo ""
echo "To actually start the UI, run:"
echo "  $ ./target/debug/joey speckit"
echo ""
echo "Or install globally:"
echo "  $ cargo install --path ."
echo "  $ joey speckit"
echo ""
echo "=========================================="
