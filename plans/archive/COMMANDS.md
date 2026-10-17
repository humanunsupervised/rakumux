# Git Worktree Manager Commands

This repository provides **two distinct interfaces** for git worktree and tmux session management.

## 🐚 `orchestra-cli` - Original Shell-Based Interface

Fast, scriptable command-line interface for git worktree operations.

### Installation

**Setup Instructions:**
Add this wrapper to your `~/.bash_profile` or `~/.zshrc`:

```bash
orchestra-cli () {
  local script="/absolute/path/to/orchestrator/orchestra-cli.sh"  # UPDATE THIS PATH
  local out
  out="$(command bash "$script" "$@")"
  local status=$?
  local cd_line
  cd_line="$(printf '%s\n' "$out" | grep -m1 '^cd \".*\"')"
  [[ -n $cd_line ]] && eval "$cd_line"
  printf '%s\n' "$out" | grep -v -F "$cd_line"
  return $status
}
```

### Commands

- **`orchestra-cli`** - Interactive fzf-based menu with tmux session management
- **`orchestra-cli ls`** - List all worktrees (command-line only)
- **`orchestra-cli ch <branch>`** - Switch to existing worktree for branch
- **`orchestra-cli ch -b <branch>`** - Create new branch + worktree and switch to it
- **`orchestra-cli b <branch>`** - Create new branch and worktree (without switching)
- **`orchestra-cli d <branch>`** - Delete worktree and local branch
- **`orchestra-cli status`** - Show current worktree context + git status
- **`orchestra-cli copy-env <target>`** - Copy env files between worktrees
- **`orchestra-cli upgrade`** - Check for available updates

### Examples
```bash
orchestra-cli ch -b feature/new-ui     # Create and switch to new feature branch
orchestra-cli ls                       # List all worktrees
orchestra-cli ch main                  # Switch to main worktree
orchestra-cli d feature/old-experiment # Delete worktree and branch
orchestra-cli                          # Interactive menu for tmux sessions
orchestra-cli upgrade                  # Check for available updates
```

## 🦀 `orchestra` - Modern Rust TUI Interface

Rich terminal UI with visual navigation and real-time updates.

### Installation

**Easy Installation (Recommended):**
```bash
# Build the Rust TUI first
./build.sh
```

**Setup Shell Integration:**
1. Build the Rust TUI: `./build.sh`
2. Add this wrapper to your `~/.bash_profile` or `~/.zshrc`:

```bash
orchestra () {
  local script="/absolute/path/to/orchestrator/orchestra.sh"  # UPDATE THIS PATH
  local out
  out="$(command bash "$script" "$@")"
  local status=$?
  local cd_line
  cd_line="$(printf '%s\n' "$out" | grep -m1 '^cd \".*\"')"
  [[ -n $cd_line ]] && eval "$cd_line"
  printf '%s\n' "$out" | grep -v -F "$cd_line"
  return $status
}
```

### Commands

- **`orchestra`** - Launch interactive TUI interface
- **`orchestra --debug`** - Launch with debug information
- **`orchestra --help`** - Show help information

### Navigation (within TUI)

- **`↑/↓`** or **`k/j`** - Navigate up/down
- **`Enter`** or **`s`** - Select worktree or attach to session
- **`Space`** - Expand/collapse worktree to show/hide sessions
- **`d`** - Delete worktree or kill session
- **`r`** - Rename session (AI-powered, when configured)
- **`c`** - Create new worktree (TODO)
- **`Ctrl+R`** - Refresh data
- **`q`** or **`Esc`** - Quit

### Features

- 🎨 **Rich Visual Interface**: Tree view with colors, icons, and hierarchical display
- ⚡ **Real-time Updates**: Live session previews and git status
- 🔍 **Visual Navigation**: Expandable worktree sessions with state persistence
- 🤖 **AI Integration**: Smart session naming (when ANTHROPIC_API_KEY configured)
- 📊 **Enhanced UX**: Better error handling, loading states, confirmation dialogs

### Examples
```bash
orchestra                    # Launch TUI interface
orchestra --debug           # Launch with debug info
```

## Quick Comparison

| Feature | `orchestra-cli` (Shell) | `orchestra` (Rust TUI) |
|---------|--------------|------------------|
| **Performance** | Fast | Very Fast |
| **Interface** | Command-line + fzf menu | Rich terminal UI |
| **Best for** | Scripting, quick operations | Interactive exploration |
| **Visual feedback** | Basic text | Colors, icons, tree view |
| **Session preview** | Basic | Real-time with formatting |
| **Dependencies** | bash, fzf, tmux | Rust binary, tmux |
| **Learning curve** | Minimal | Slightly higher |
| **Scriptability** | Excellent | Limited |

## Building and Prerequisites

### For `orchestra-cli` (Shell Interface)
- Git 2.5+ 
- bash
- fzf (for interactive mode)
- tmux (optional, for session management)

### For `orchestra` (Rust TUI)
- All of the above, plus:
- Rust 1.70+ (for building)
- jq (for JSON processing)

### Build Process
```bash
./build.sh              # Build release version
./build.sh --debug      # Build debug version  
./build.sh --install    # Build and install globally
```

## Choosing Between `orchestra-cli` and `orchestra`

### Use `orchestra-cli` when:
- Writing scripts or automation
- Quick, one-off operations
- Prefer command-line efficiency
- Working in CI/CD environments
- Minimal dependencies preferred

### Use `orchestra` when:
- Interactive worktree exploration
- Managing complex session hierarchies
- Visual feedback is important
- Working with many worktrees/sessions
- Best user experience is priority

Both interfaces use the same underlying Git and tmux APIs, so you can seamlessly switch between them based on your current workflow needs.

## Cross-Repository Usage

Both `orchestra-cli` and `orchestra` can be used from any git repository on your system once installed:

```bash
# Install once from orchestrator repo
cd /path/to/orchestrator
./build.sh                       # Build the binary
# Add shell wrappers to ~/.bashrc or ~/.zshrc (see Setup Instructions)

# Then use from any git repository
cd /path/to/my-project
orchestra-cli ls                    # Works from any repo
orchestra                     # Works from any repo
```

The commands automatically:
- ✅ Find the orchestrator installation (services.sh, binaries)
- ✅ Operate on the current repository's worktrees and branches
- ✅ Handle paths and directories correctly across repositories
- ✅ Maintain tmux sessions per repository

### Build and Setup

```bash
./build.sh              # Build Rust TUI binary
./orchestra.sh                # Test TUI directly
./orchestra-cli.sh --help          # Show CLI help
```

For shell integration, see the Setup Instructions section above.
