#!/bin/bash

# Find orchestrator root directory (parent of tests/)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORCHESTRATOR_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== TUI Debug Test ==="
echo "Orchestrator root: $ORCHESTRATOR_ROOT"
echo ""

echo "1. Testing if TUI starts and exits properly:"
echo "   Running: echo 'q' | orchestra"
echo "   (Should start TUI, send 'q' key, and exit)"
echo ""

# Run from the orchestrator directory to ensure proper context
cd "$ORCHESTRATOR_ROOT"
echo 'q' | "$ORCHESTRATOR_ROOT/orchestra.sh" 2>/dev/null

echo ""
echo "2. Testing basic terminal capabilities:"
echo "   Terminal: $TERM"
echo "   Size: $(tput cols)x$(tput lines)"
echo "   Colors: $(tput colors)"
echo ""

echo "3. Testing if alternate screen works:"
echo "   Switching to alternate screen for 2 seconds..."
tput smcup  # Enter alternate screen
sleep 1
echo "You should see this in alternate screen mode"
sleep 1
tput rmcup  # Exit alternate screen
echo "   Back to normal screen"
echo ""

echo "4. Testing colors:"
echo -e "   \033[31mRed\033[0m \033[32mGreen\033[0m \033[34mBlue\033[0m \033[33mYellow\033[0m"
echo ""

echo "If the TUI is completely invisible, the issue is likely:"
echo "  - Terminal colors (background same as UI colors)"  
echo "  - Alternate screen mode not working"
echo "  - Terminal escape sequence compatibility"
