#!/bin/bash

# Setup Public Repository for Closed-Source Distribution
# Creates the minimal public repository (README + LICENSE) in a temp directory.
# LICENSE is copied from install/npm/LICENSE, the single canonical copy shipped
# in every channel (npm package, release tarballs, Homebrew).
set -e

VERSION=${1:-"0.1.0"}
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LICENSE_SOURCE="$SCRIPT_DIR/../npm/LICENSE"

if [[ ! -f "$LICENSE_SOURCE" ]]; then
    echo "❌ License file not found: $LICENSE_SOURCE"
    exit 1
fi

echo "🌟 Setting up minimal public repository for Orchestra..."

# Create temporary directory for public repo
TEMP_DIR=$(mktemp -d)
cd "$TEMP_DIR"

echo "📝 Creating minimal public repository structure..."

# Initialize git repository
git init
git config user.name "$(git config --global user.name)"
git config user.email "$(git config --global user.email)"

cat > README.md << 'EOF'
# Orchestra

**Terminal sessions for coding agents, in one fast TUI.**

Orchestra keeps all your terminal sessions, in every folder and worktree, in one place: a session
list on the left, the live terminal on the right. Sessions run in a small background daemon
(`orchestra-ptyd`), so they keep running when you close the TUI and come back exactly where you left
them. Built for working with Claude Code, Codex, OpenCode and other coding agents in parallel.

## Install

### npm
```bash
npm install -g @humanu/orchestra
```

### Homebrew (macOS and Linux)
```bash
brew install humanunsupervised/orchestra/orchestra
```

### Release tarball
```bash
# macOS Apple Silicon: orchestra-macos-arm64 · macOS Intel: orchestra-macos-intel · Linux x64: orchestra-linux-x64
curl -L https://github.com/humanunsupervised/orchestra/releases/latest/download/orchestra-macos-arm64.tar.gz | tar xz
cd orchestra-macos-arm64 && ./install.sh
```

## Usage

```bash
orchestra                      # Open the TUI for the current folder
orchestra new --name dev       # Create a session and attach to it
orchestra ls                   # List sessions in this folder
orchestra attach dev           # Full-screen attach (Ctrl-\ d to detach)
orchestra --help               # All commands
```

Open Help inside the TUI for keys.

## AI session names

Set an Anthropic or OpenAI API key (inside the TUI, or `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`)
and Orchestra names new sessions from what you run in them.

## Requirements

- macOS (Apple Silicon or Intel) or Linux x64
- bash

No git, tmux or other runtime dependencies.

## License

Orchestra is proprietary software; see [LICENSE](LICENSE). Each package also contains
`THIRD_PARTY_NOTICES` with the licenses of the open-source components it includes.

## Support

For issues and feature requests, please create an issue in this repository.
EOF

# LICENSE: the canonical text shipped with every package
cp "$LICENSE_SOURCE" LICENSE

# Create .gitignore
cat > .gitignore << 'EOF'
# Release artifacts (if any accidentally added)
*.tar.gz
*.zip

# macOS
.DS_Store

# Editor files
.vscode/
*.swp
*.swo
*~

# Temporary files
tmp/
temp/
EOF

# Add all files
git add .

# Commit
git commit -m "Initial public repository for Orchestra v$VERSION

Closed-source distribution repository containing:
- Installation instructions
- License terms
- Public support information

Binaries distributed via GitHub Releases only.
Source code remains private."

echo "✅ Public repository structure created in: $TEMP_DIR"
echo ""
echo "📋 Repository contents:"
ls -la
echo ""
echo "🔗 Next steps:"
echo "1. Push to public repository:"
echo "   cd $TEMP_DIR"
echo "   git remote add origin git@github.com:humanunsupervised/orchestra.git"
echo "   git push -u origin main"
echo ""
echo "2. Build release packages:"
echo "   ./install/scripts/build-release.sh $VERSION"
echo ""
echo "3. Create GitHub release with the packages"

# Keep the directory for manual push
echo ""
echo "📂 Temporary directory: $TEMP_DIR"
echo "   (Directory will persist for manual operations)"
