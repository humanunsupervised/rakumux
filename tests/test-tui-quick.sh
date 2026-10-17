#!/bin/bash
echo "Testing TUI with brief run..."
echo "This should show the UI and exit quickly"

# Start TUI and kill it after a few seconds 
./rakumux.sh --debug &
TUI_PID=$!
sleep 3
kill $TUI_PID 2>/dev/null
wait $TUI_PID 2>/dev/null
echo "TUI test completed"
