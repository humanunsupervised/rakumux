#!/bin/bash

echo "=== Testing the orchestra wrapper command ==="
echo ""

echo "1. Checking if the wrapper is defined in the current shell:"
if type orchestra >/dev/null 2>&1; then
    echo "✅ orchestra command is defined"
else
    echo "❌ orchestra command is NOT defined"
fi

echo ""
echo "2. Testing the launcher directly:"
echo "orchestra.sh --help:"
./orchestra.sh --help | head -3

echo ""
echo "3. Checking what's in your shell profile:"
if [[ -f "$HOME/.bash_profile" ]]; then
    echo "Last 20 lines of ~/.bash_profile:"
    tail -20 "$HOME/.bash_profile" | grep -E "orchestra" | head -5 || true
else
    echo "~/.bash_profile not found; skipping shell profile check"
fi

echo ""
echo "4. Instructions to fix:"
echo "If the function is not defined, run:"
echo "  source ~/.bash_profile"
echo ""
echo "If that doesn't work, check your shell configuration manually."
