#!/bin/bash

# Test script to verify arrow key navigation in the TUI

echo "Testing Rust TUI key navigation..."
echo "This will launch the TUI briefly to test key handling"

# Start TUI in debug mode in background
timeout 5s ./orchestra.sh --debug &
TUI_PID=$!

# Give it a moment to start
sleep 1

# Send some test keypresses (this won't work perfectly but let's see if it starts)
# The real test will be manual

wait $TUI_PID
echo "TUI test completed"
