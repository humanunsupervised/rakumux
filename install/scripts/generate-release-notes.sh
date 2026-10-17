#!/bin/bash

# Generate intelligent release notes by analyzing commits since last public release
# Outputs Markdown to stdout. Safe fallbacks if API or history not available.

set -euo pipefail

REPO_SLUG="humanunsupervised/orchestra"
VERSION=""
NOTES_LAST_COMMITS=""
FALLBACK_COMMITS=5

log() { printf "[notes] %s\n" "$*" >&2; }
warn() { printf "[notes][warn] %s\n" "$*" >&2; }
err() { printf "[notes][error] %s\n" "$*" >&2; }

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --repo=*)
        REPO_SLUG="${1#*=}"
        shift
        ;;
      --notes-last=*)
        NOTES_LAST_COMMITS="${1#*=}"
        shift
        ;;
      *)
        if [[ -z "$VERSION" ]]; then
          VERSION="$1"
        else
          warn "Ignoring unexpected argument: $1"
        fi
        shift
        ;;
    esac
  done

  if [[ -n "$NOTES_LAST_COMMITS" ]] && [[ ! "$NOTES_LAST_COMMITS" =~ ^[1-9][0-9]*$ ]]; then
    err "Invalid --notes-last value: $NOTES_LAST_COMMITS (expected positive integer)"
    exit 1
  fi
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || { err "Missing required command: $1"; return 1; }
}

is_ancestor_of_head() {
  local ref="$1"
  [[ -n "$ref" ]] || return 1
  git rev-parse -q --verify "${ref}^{commit}" >/dev/null 2>&1 || return 1
  git merge-base --is-ancestor "$ref" HEAD >/dev/null 2>&1
}

# Determine repo root (if inside a git repo)
repo_root() { git rev-parse --show-toplevel 2>/dev/null || true; }

# Load env file and export variables
load_env_file() {
  local env_file="$1"
  [[ -f "$env_file" ]] || return 0
  set +u
  set -a
  # shellcheck disable=SC1090
  source "$env_file"
  set +a
  set -u
}

# Load the Anthropic API key: environment, ~/.orchestra/config.json, then .env files
load_anthropic_api_key() {
  # If already set, keep it
  if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
    return 0
  fi

  # Prefer ~/.orchestra/config.json (jq required)
  local config_file="$HOME/.orchestra/config.json"
  if [[ -f "$config_file" ]] && command -v jq >/dev/null 2>&1; then
    local api_key
    api_key="$(jq -r '.anthropic_api_key // empty' "$config_file" 2>/dev/null || echo "")"
    if [[ -n "$api_key" && "$api_key" != "null" ]]; then
      export ANTHROPIC_API_KEY="$api_key"
      return 0
    fi
  fi

  # Fallback to .env in CWD
  load_env_file "$PWD/.env"

  # Fallback to repo root .env
  if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
    local root
    root="$(repo_root)"
    if [[ -n "$root" ]]; then
      load_env_file "$root/.env"
    fi
  fi
}

# Get latest release metadata (tag and published_at). Returns two lines: tag and ISO date
get_latest_release() {
  require_cmd gh || return 1
  gh api \
    repos/"$REPO_SLUG"/releases/latest \
    --jq '.tag_name, .published_at' 2>/dev/null
}

# Get latest local semver tag (ignores malformed tags like vpatch)
latest_local_semver_tag() {
  git tag -l 'v[0-9]*.[0-9]*.[0-9]*' --sort=-version:refname | head -n1
}

# Tracking tags are created by full-release-automation.sh after successful releases.
latest_tracking_tag() {
  git tag -l 'private-release/v[0-9]*.[0-9]*.[0-9]*' --sort=-version:refname | head -n1
}

# Get commit subjects since a given ref or date
# Priority: if from_ref exists locally → use range from_ref..HEAD
# Else if published_at provided → use --since
get_commits_since() {
  local from_ref="$1"
  local published_at="$2"

  if [[ -n "$from_ref" ]] && git rev-parse -q --verify "${from_ref}^{commit}" >/dev/null 2>&1; then
    git log --pretty=format:%s "$from_ref"..HEAD || true
  elif [[ -n "$published_at" ]]; then
    git log --since="$published_at" --pretty=format:%s || true
  else
    return 0
  fi
}

get_last_n_commits() {
  local limit="$1"
  git log -n "$limit" --pretty=format:%s || true
}

# Call Anthropic API to summarize commits into concise bullet points
anthropic_summarize() {
  local commits="$1"
  local version="$2"

  load_anthropic_api_key
  if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
    return 1
  fi

  # Build JSON using jq if available, else here-doc escaped
  if command -v jq >/dev/null 2>&1; then
    local system_prompt="You are a release notes generator for a developer tool named Orchestra. Given a list of git commit subjects since the last public release, produce concise, user-focused bullet points. Group similar items, highlight important changes, and omit purely internal refs unless impactful. Prefer sections: What's New, Improvements, Bug Fixes, Breaking Changes (only if present). Keep it short and specific."

    local request
    request=$(jq -n --arg model "claude-3-5-haiku-20241022" \
                 --arg system "$system_prompt" \
                 --arg commits "$commits" \
                 --arg version "$version" \
                 '{
                   model: $model,
                   max_tokens: 600,
                   temperature: 0.2,
                   system: $system,
                   messages: [
                     { role: "user", content: ("Generate Markdown release notes for Orchestra v" + $version + " based on these commit subjects since the last release.\n\nCommit subjects (one per line):\n" + ($commits | split("\n") | map(select(length>0) | "- " + .) | join("\n")) + "\n\nOutput:\n- A brief intro line (one sentence).\n- Then 5-12 bullet points grouped under headings if helpful.\n- Avoid generic phrasing. Be specific and user-facing.\n- No backticks, no code blocks, plain Markdown.") }
                   ]
                 }')
  else
    # Minimal JSON without jq; risk of escaping issues if commits include quotes
    local safe_commits
    safe_commits=$(printf '%s' "$commits" | sed 's/"/\"/g; s/^/- /')
    request=$(cat <<JSON
{
  "model": "claude-3-5-haiku-20241022",
  "max_tokens": 600,
  "temperature": 0.2,
  "system": "You are a release notes generator for a developer tool named Orchestra. Given a list of git commit subjects since the last public release, produce concise, user-focused bullet points. Group similar items, highlight important changes, and omit purely internal refs unless impactful. Prefer sections: What's New, Improvements, Bug Fixes, Breaking Changes (only if present). Keep it short and specific.",
  "messages": [
    {
      "role": "user",
      "content": "Generate Markdown release notes for Orchestra v$version based on these commit subjects since the last release.\\n\\nCommit subjects (one per line):\\n$safe_commits\\n\\nOutput:\\n- A brief intro line (one sentence).\\n- Then 5-12 bullet points grouped under headings if helpful.\\n- Avoid generic phrasing. Be specific and user-facing.\\n- No backticks, no code blocks, plain Markdown."
    }
  ]
}
JSON
)
  fi

  local response
  response=$(curl -sS https://api.anthropic.com/v1/messages \
    -H "x-api-key: $ANTHROPIC_API_KEY" \
    -H "anthropic-version: 2023-06-01" \
    -H "content-type: application/json" \
    -d "$request") || return 1

  # Extract text content
  if command -v jq >/dev/null 2>&1; then
    jq -r '.content[0].text // empty' <<<"$response"
  else
    # naive extraction
    printf '%s' "$response" | sed -n 's/.*"text":"\([^"]*\)".*/\1/p' | head -n1 | sed 's/\\n/\n/g'
  fi
}

main() {
  parse_args "$@"

  require_cmd git || { err "git is required"; exit 1; }
  require_cmd gh || warn "gh not found or not authenticated; attempting best-effort without remote release context"

  # Fetch latest release info (tag and published date)
  local latest_tag published_at local_tag tracking_tag from_ref commits out
  latest_tag=""
  published_at=""
  if out=$(get_latest_release 2>/dev/null); then
    latest_tag="$(printf '%s' "$out" | sed -n '1p')"
    published_at="$(printf '%s' "$out" | sed -n '2p')"
  fi

  local_tag="$(latest_local_semver_tag)"
  tracking_tag="$(latest_tracking_tag)"
  from_ref=""
  commits=""

  if [[ -n "$NOTES_LAST_COMMITS" ]]; then
    log "Using explicit commit limit: last $NOTES_LAST_COMMITS commits"
    commits="$(get_last_n_commits "$NOTES_LAST_COMMITS")"
  else
    if [[ -n "$tracking_tag" ]]; then
      if is_ancestor_of_head "$tracking_tag"; then
        from_ref="$tracking_tag"
        log "Using tracking tag baseline: $from_ref"
      else
        warn "Ignoring tracking tag baseline ($tracking_tag): not an ancestor of HEAD."
      fi
    fi

    if [[ -z "$from_ref" ]] && [[ -n "$latest_tag" ]] && is_ancestor_of_head "$latest_tag"; then
      from_ref="$latest_tag"
      log "Using matching public release tag baseline: $from_ref"
    fi

    if [[ -z "$from_ref" ]] && [[ -n "$local_tag" ]]; then
      if is_ancestor_of_head "$local_tag"; then
        if [[ -n "$latest_tag" ]] && [[ "$local_tag" != "$latest_tag" ]]; then
          warn "Latest local semver tag ($local_tag) differs from latest public release ($latest_tag); using published release timestamp baseline instead."
        else
          from_ref="$local_tag"
          log "Using local release tag baseline: $from_ref"
        fi
      else
        warn "Ignoring local release tag baseline ($local_tag): not an ancestor of HEAD."
      fi
    fi

    if [[ -z "$commits" ]]; then
      if [[ -z "$from_ref" ]] && [[ -n "$published_at" ]]; then
        log "Using latest public release timestamp baseline: $published_at"
      fi
      commits="$(get_commits_since "$from_ref" "$published_at")"
    fi

    if [[ -z "${commits//[[:space:]]/}" ]] && [[ -z "$from_ref" ]] && [[ -z "$published_at" ]]; then
      warn "No release baseline found; using last $FALLBACK_COMMITS commits."
      commits="$(get_last_n_commits "$FALLBACK_COMMITS")"
    fi
  fi

  # Header
  echo "## Orchestra v${VERSION:-(unreleased)}"
  echo

  if [[ -z "${commits//[[:space:]]/}" ]]; then
    echo "- Maintenance release"
    return 0
  fi

  # Simple, succinct list of one-line commit subjects (features and fixes)
  local filtered
  filtered="$(printf '%s\n' "$commits" | grep -E '^(feat|fix)(\(|!|:)' || true)"

  # If no conventional feat/fix commits found, fall back to all subjects
  if [[ -z "${filtered//[[:space:]]/}" ]]; then
    filtered="$commits"
  fi

  printf '%s\n' "$filtered" | while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    echo "- $line"
  done
}

main "$@"
