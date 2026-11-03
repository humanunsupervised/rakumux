#!/bin/bash

# Check GitHub CLI Authentication
# Ensures gh is properly authenticated before running release scripts

check_gh_auth() {
    echo "🔍 Checking GitHub CLI authentication..."
    
    # Check if gh is installed
    if ! command -v gh >/dev/null 2>&1; then
        echo "❌ GitHub CLI not installed"
        echo "Install with: brew install gh"
        return 1
    fi
    
    # Check authentication status
    if gh auth status >/dev/null 2>&1; then
        echo "✅ GitHub CLI is authenticated"
        gh auth status
        return 0
    else
        echo "❌ GitHub CLI not authenticated"
        echo ""
        echo "🔧 Quick Setup Options:"
        echo ""
        echo "Option 1 - Personal Access Token (Recommended):"
        echo "  1. Go to: https://github.com/settings/tokens"
        echo "  2. Generate new token with 'repo' scope"
        echo "  3. Run: echo 'your_token' | gh auth login --with-token"
        echo ""
        echo "Option 2 - SSH (One-time setup):"
        echo "  gh auth login --git-protocol ssh"
        echo "  (Choose SSH and your existing key)"
        echo ""
        echo "Option 3 - Environment Variable:"
        echo "  export GITHUB_TOKEN='your_token'"
        echo "  (Add to ~/.bashrc or ~/.zshrc)"
        echo ""
        return 1
    fi
}

# Run the check
check_gh_auth