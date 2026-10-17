# Update Flow Test Plan

This document describes how to test the automatic update feature for Orchestra.

## How the Update System Works

### Flow Overview:

1. **Startup Check**: When the app starts, it checks if an update is available (once per 24 hours max)
2. **Toast Notification**: If a newer version is detected, show message: "Orchestra will upgrade to version X.X.X next time you restart"
3. **Auto-Upgrade on Next Start**: The next time the app is launched, it automatically runs the upgrade command and exits
4. **User Restarts**: User restarts the app to use the new version

### Implementation Details:

- **State File**: `~/.orchestra/update_state.json` tracks:
  - `last_check`: timestamp of last update check
  - `latest_version`: the latest version available
  - `pending_upgrade`: whether an upgrade should happen on next start
  - `install_method`: "npm" or "homebrew"

- **Update Check**: Runs in background on app startup (non-blocking)
  - Detects install method via `which orchestra` path
  - For npm: runs `npm view @humanu/orchestra version`
  - For homebrew: runs `brew info --json orchestra`

- **Upgrade Execution**: Before TUI starts, if `pending_upgrade` is true:
  - For npm: runs `npm install -g @humanu/orchestra`
  - For homebrew: runs `brew upgrade orchestra`
  - Clears `pending_upgrade` flag
  - Exits with message to restart

## Manual Testing Steps

### Test 1: Initial Setup
```bash
# Remove any existing state
rm -f ~/.orchestra/update_state.json

# Start the app
orchestra  # or orchestra

# Expected: App starts normally (no update check message yet)
```

### Test 2: Simulate Available Update
```bash
# Edit the update state file to simulate a pending update
cat > ~/.orchestra/update_state.json <<'JSON'
{
  "last_check": null,
  "latest_version": "0.6.0",
  "pending_upgrade": false,
  "install_method": "npm"
}
JSON

# Start the app
orchestra

# Expected: After splash screen, see message:
# "Orchestra will upgrade to version 0.6.0 next time you restart"

# Exit the app (Esc)
```

### Test 3: Auto-Upgrade on Next Start
```bash
# The previous step should have set pending_upgrade=true
# Verify the state:
cat ~/.orchestra/update_state.json

# Start the app
orchestra

# Expected:
# - Message: "Upgrading Orchestra to version 0.6.0..."
# - Message: "Upgrade initiated successfully. The app will now exit."
# - Message: "Please restart Orchestra to use the new version."
# - App exits WITHOUT showing TUI
```

### Test 4: Skip Check Within 24 Hours
```bash
# Set a recent last_check timestamp
cat > ~/.orchestra/update_state.json <<JSON
{
  "last_check": $(date +%s),
  "latest_version": null,
  "pending_upgrade": false,
  "install_method": null
}
JSON

# Start the app
orchestra

# Expected: App starts normally, no update check (within 24 hour window)
```

## Automated Test

To simulate the full flow without waiting for actual npm/brew:

```bash
# Manually set an older version in Cargo.toml temporarily
# Then run the update detection logic

# Reset state
rm -f ~/.orchestra/update_state.json

# This should trigger the check and mark pending
orchestra

# Verify state was updated
cat ~/.orchestra/update_state.json
```

## Edge Cases to Test

1. **No network**: Update check should fail gracefully
2. **npm/brew not installed**: Should detect "Unknown" install method and skip
3. **Installed from source**: Should skip update checks
4. **Version is already latest**: Should not show notification

## Success Criteria

✅ Update check happens max once per 24 hours
✅ Toast notification shows when update is available
✅ Pending upgrade executes on next start before TUI loads
✅ App exits cleanly after initiating upgrade
✅ User must restart to use new version
✅ No update check when installed from source
✅ Graceful handling of network/command failures
