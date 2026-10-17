# Session Preview Refresh Bug Fix

## Problem Summary

The session preview was refreshing too frequently, causing performance issues and unnecessary resource usage. Every time the user navigated between sessions (even within the same worktree), the preview would restart from scratch, leading to:

1. **Excessive network/API calls** - Preview polling restarted every time cursor moved
2. **Poor UX** - Previews flickering/reloading constantly during navigation
3. **Wasted resources** - Unnecessary tmux capture-pane calls and SSH connections
4. **No caching** - Previously loaded previews were discarded and re-fetched

## Root Causes

### 1. Over-Aggressive Preview Polling Stop (refresh.rs:354)
**Issue**: `mark_navigation_occurred()` called `stop_preview_polling()` on EVERY navigation event, even when moving between sessions.

**Impact**: Any cursor movement (↑/↓ keys) would abort the active preview polling task, forcing a complete restart.

### 2. Missing Cache Check (session_preview.rs:13-31)
**Issue**: `load_session_preview_if_needed()` didn't check if a cached preview already existed before starting a new polling task.

**Impact**: Even if we had recently loaded a preview for a session, navigating away and back would trigger a full re-poll.

### 3. Unnecessary Polling Restarts
**Issue**: The debounce logic worked correctly (200ms delay), but once triggered, it would ALWAYS start a new preview poll regardless of whether:
- We already had a cached preview
- We were already polling that exact session
- The session was recently viewed

## Solution

### Change 1: Smart Preview Cache Check (session_preview.rs:22-29)
```rust
// Check if we already have a cached preview for this session
// Only re-poll if the preview doesn't exist
if app.session_previews.contains_key(&session_name) {
    // We have a cached preview, no need to poll again
    // Stop any active polling for the previous session
    if app.preview_polling_session.is_some() {
        stop_preview_polling(app);
    }
    return;
}
```

**Benefit**: Cached previews are reused when navigating back to previously viewed sessions. No unnecessary re-polling.

### Change 2: Conditional Preview Polling Stop (refresh.rs:354-365)
```rust
// Only stop preview polling if we're navigating away from a session to a non-session item
// This allows moving between sessions without stopping the polling unnecessarily
let items = app.get_display_items();
let navigating_to_session = matches!(
    items.get(app.selected_index),
    Some(super::types::DisplayItem::Session { .. })
);

if !navigating_to_session {
    // Only stop if we're moving to a worktree or new session button
    session_preview::stop_preview_polling(app);
}
```

**Benefit**: Preview polling continues when navigating between sessions, only stops when moving to worktree items or "new session" buttons.

## Behavior Changes

### Before Fix:
```
User navigates: Session A → Session B → Session A
  ↓
Session A preview loads (polling starts)
  ↓
User moves to Session B (200ms later)
  ↓ 
Session A polling STOPS (aborted)
  ↓
Session B preview loads (new polling starts)
  ↓
User moves back to Session A
  ↓
Session B polling STOPS (aborted)
  ↓
Session A preview RE-LOADS (unnecessary!)
```

**Result**: Every navigation caused a full preview reload, even for previously viewed sessions.

### After Fix:
```
User navigates: Session A → Session B → Session A
  ↓
Session A preview loads (polling starts → stops when idle)
  ↓
User moves to Session B (200ms later)
  ↓
Session A preview stays CACHED
Session B preview loads (new polling starts → stops when idle)
  ↓
User moves back to Session A
  ↓
Session A preview shown from CACHE (no re-polling!)
```

**Result**: Previews are cached and reused. Only new sessions trigger polling.

## Performance Impact

### Preview Refresh Frequency
- **Before**: Every 200ms + debounce on cursor movement = **constant refreshing**
- **After**: Only on first view of a session = **one-time load per session**

### Network/SSH Calls (Remote Sessions)
- **Before**: Unlimited SSH calls as user navigates
- **After**: Max 1 SSH polling session per tmux session (until idle)

### Local tmux Calls
- **Before**: Continuous `tmux capture-pane` calls during navigation
- **After**: Polls until session is idle (2 consecutive idle states = ~1 second), then stops

### Cache Behavior
- **Before**: No caching - every navigation triggers fresh load
- **After**: Intelligent caching - previews persist across navigation

## Testing Recommendations

1. **Cache Persistence Test**:
   - Navigate to Session A (wait for preview to load)
   - Navigate to Session B (wait for preview to load)  
   - Navigate back to Session A
   - **Expected**: Session A preview appears instantly from cache

2. **Idle Detection Test**:
   - Select a session with a shell prompt (idle)
   - Wait ~1 second after preview loads
   - **Expected**: Preview polling stops after 2 idle states detected

3. **Active Session Test**:
   - Select a session running a long command (non-idle)
   - Observe preview updates
   - **Expected**: Preview continues polling until command finishes

4. **Memory Test**:
   - Navigate through 10+ sessions
   - Check memory usage
   - **Expected**: Previews cached in memory, no growth from repeated polling

## Files Modified

1. `tui/src/app/session_preview.rs` - Added cache check before polling
2. `tui/src/app/refresh.rs` - Made preview stop conditional on navigation target

## Debounce Still Active

The 200ms navigation debounce (`NAV_DEBOUNCE_MS` constant) remains unchanged and working correctly. This fix doesn't affect debouncing - it prevents unnecessary refreshes AFTER the debounce completes.

## Future Improvements (Optional)

1. **TTL-based cache invalidation**: Add timestamps to cached previews and invalidate after X seconds
2. **Manual refresh shortcut**: Add key binding (e.g., `Ctrl-R`) to force preview refresh
3. **Cache size limit**: Limit preview cache to last N sessions to prevent unbounded memory growth
4. **Selective cache clear**: Clear preview cache when sessions are killed/renamed
