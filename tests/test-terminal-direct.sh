#!/bin/bash

echo "=== Direct Terminal Test ==="
echo "Testing if we can write directly to alternate screen..."
echo ""
echo "This test will:"
echo "1. Switch to alternate screen"
echo "2. Write visible text directly"
echo "3. Wait for 'q' key"
echo "4. Return to normal screen"
echo ""
echo "Press Enter to start..."
read

# Switch to alternate screen
printf '\033[?1049h'

# Clear screen and position cursor
printf '\033[2J\033[H'

# Write some very visible content
printf '\033[41;33m'  # Red background, yellow text
printf '████████████████████████████████████████████████████████\n'
printf '█                                                      █\n'
printf '█  DIRECT TERMINAL TEST - CAN YOU SEE THIS TEXT?      █\n'
printf '█                                                      █\n'
printf '█  If you can see this, the terminal works fine       █\n'
printf '█  If not, there is a fundamental terminal issue      █\n'
printf '█                                                      █\n'
printf '█  Press q to exit                                    █\n'
printf '█                                                      █\n'
printf '████████████████████████████████████████████████████████\n'
printf '\033[0m'  # Reset colors

# Position cursor and add more text
printf '\033[12;5H'
printf '\033[44;37m Terminal size: %dx%d \033[0m' $(tput cols) $(tput lines)

printf '\033[14;5H'
printf '\033[42;30m Working directory: %s \033[0m' "$PWD"

printf '\033[16;5H'
printf '\033[43;31m Current time: %s \033[0m' "$(date)"

# Wait for 'q' key
while IFS= read -r -n1 char; do
    if [[ $char == "q" || $char == "Q" ]]; then
        break
    fi
done

# Return to normal screen
printf '\033[?1049l'

echo ""
echo "Test completed. Could you see the red box with yellow text?"