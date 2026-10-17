# Automatic Update System Implementation

## Overview

Orchestra now includes an automatic update system that checks for newer versions and seamlessly upgrades the installation.

## User Experience Flow

1. **User starts Orchestra**
2. **System checks for updates** (once per 24 hours)
3. **If newer version detected**: Toast message appears: "Orchestra will upgrade to version X.X.X next time you restart"
4. **User exits and restarts Orchestra**
5. **Auto-upgrade executes**: 
   - Shows "Upgrading Orchestra to version X.X.X..."
   - Runs appropriate upgrade command (npm/brew)
   - Shows "Upgrade initiated successfully. The app will now exit."
   - Shows "Please restart Orchestra to use the new version."
   - App exits
6. **User restarts Orchestra again** → Now running latest version

## Implementation Details

### New Files Created

#### 1. `tui/src/updater.rs`
Core update checking and upgrade logic:

**Key Types:**
- `UpdateState`: Persisted state tracking last check, available version, pending upgrade
- `InstallMethod`: Enum for npm, homebrew, source, or unknown installations

**Key Functions:**
- `detect_install_method()`: Checks `which orchestra` output to determine installation method
- `get_latest_version(method)`: Queries npm or brew for latest version
- `compare_versions(current, latest)`: Semantic version comparison
- `check_for_updates(current_version)`: Background check returning (version, method) if update available
- `perform_upgrade(method)`: Executes appropriate upgrade command

**State File:** `~/.orchestra/update_state.json`
```json
{
  "last_check": 1696723200,
  "latest_version": "0.6.0",
  "pending_upgrade": true,
  "install_method": "npm"
}
```

### Modified Files

#### 2. `tui/src/main.rs`
- Added `mod updater`
- **Pre-TUI Check**: Before starting the TUI, checks if `pending_upgrade` is true
  - If yes: runs upgrade command, clears flag, shows messages, exits
  - If no: continues to TUI as normal

#### 3. `tui/src/app.rs`
- **Background Update Check**: Spawns async task on app initialization:
  - Checks if 24 hours have passed since last check
  - Queries npm/brew for latest version
  - Compares with current version
  - Updates state file if newer version found

#### 4. `tui/src/app/handlers/splash.rs`
- **Toast Notification Logic**: When transitioning from splash to browsing mode:
  - Checks update state
  - If newer version available: marks `pending_upgrade = true` and shows toast
  - Message format: "Orchestra will upgrade to version X.X.X next time you restart"

## Install Method Detection

The system intelligently detects how Orchestra was installed:

```rust
pub fn detect_install_method() -> InstallMethod {
    // Check `which orchestra` output path
    if path.contains("node_modules") || path.contains(".npm") || path.contains(".nvm") {
        return InstallMethod::Npm;
    }
    if path.contains("Cellar") || path.contains("Homebrew") {
        return InstallMethod::Homebrew;
    }
    if path.contains("target/release") || path.contains("target/debug") {
        return InstallMethod::Source;  // Skip updates for local development
    }
    InstallMethod::Unknown  // Skip updates if unsure
}
```

## Version Checking

### For npm:
```bash
npm view @humanu/orchestra version
# Returns: "0.5.38"
```

### For Homebrew:
```bash
brew info --json orchestra | python3 -c "import sys, json; print(json.load(sys.stdin)[0]['versions']['stable'])"
# Returns: "0.5.38"
```

## Upgrade Execution

### For npm:
```bash
npm install -g @humanu/orchestra
```

### For Homebrew:
```bash
brew upgrade orchestra
```

Both commands are spawned as child processes (non-blocking) so the app can exit immediately after initiating the upgrade.

## Safety Features

1. **24-hour check throttle**: Prevents excessive network requests
2. **Source install detection**: Skips updates for local development
3. **Graceful failure handling**: Network errors don't crash the app
4. **State persistence**: Uses JSON file for reliability
5. **Non-blocking checks**: Update check runs in background, doesn't delay startup
6. **Two-step upgrade**: Notification → pending flag → upgrade on restart

## Testing

See `test-update-flow.md` for detailed test scenarios.

Quick test:
```bash
# Simulate available update
cat > ~/.orchestra/update_state.json <<'JSON'
{
  "latest_version": "0.6.0",
  "pending_upgrade": false,
  "install_method": "npm"
}
JSON

# Start app → see toast notification
orchestra

# Exit and restart → auto-upgrade runs
orchestra
```

## Benefits

✅ **User-friendly**: No manual intervention required
✅ **Non-intrusive**: Check happens in background
✅ **Safe**: Two-step process with clear user communication
✅ **Smart**: Detects installation method automatically
✅ **Efficient**: Checks max once per 24 hours
✅ **Reliable**: Persisted state survives app crashes

## Future Enhancements

Potential improvements:
- Option to skip/postpone update
- Release notes preview
- Configurable check frequency
- Beta/stable channel selection
