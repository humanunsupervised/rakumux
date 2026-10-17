# install/ - Closed-source distribution of Orchestra

Orchestra ships as four native binaries plus a small bash launcher, through three channels (npm,
Homebrew, release tarballs) that all behave the same way. Source code never leaves the private
repository.

## Repositories

| Repository | Visibility | Contents |
|---|---|---|
| `adeperio/orchestrator` | private | Source, build and release scripts (this repo) |
| `humanunsupervised/orchestra` | public | README, `LICENSE`, GitHub Releases (tarballs + `checksums.txt`) |
| `humanunsupervised/homebrew-orchestra` | public | `Formula/orchestra.rb` (the tap) |
| npm `@humanu/orchestra` | public | Package staged from the release tarballs |

## What ships

Every release tarball `orchestra-<platform>.tar.gz` (`macos-arm64`, `macos-intel`, `linux-x64`)
contains `orchestra-<platform>/`:

| Path | Purpose |
|---|---|
| `orchestra` | The `tui` binary: TUI with no command, CLI subcommands otherwise |
| `orchestra-ptyd` | Session daemon (Cargo workspace member `tui/crates/ptyd`), spawned on demand by clients |
| `orchestra-api` | External HTTP API on 127.0.0.1 (workspace member `api/`), started on demand by `orchestra-mcp` |
| `orchestra-mcp` | MCP server (stdio) for coding agents (workspace member `mcp/`) |
| `orchestra.sh` | Launcher every channel runs, for every argument |
| `shell/gwr_*.sh`, `shell/gwr/check-updates.sh` | Launcher modules |
| `shell/orchestra-command-hook.sh` | Command-history hook for sessions (AI naming input) |
| `orchestra-local.sh` | Launches a local source checkout (development) |
| `LICENSE` | Orchestra license (proprietary), from `install/npm/LICENSE` |
| `THIRD_PARTY_NOTICES` | Licenses of every crate linked into the binaries |
| `install.sh`, `README.md` | Tarball installer and package readme |

Runtime dependencies: bash only. No git, tmux, jq, python3 or services layer; `env-copy` and
`orchestra-cli` were removed in 0.6.0 and must not ship again (`test-install.sh` and
`publish-npm.sh` fail if they do).

## Launch path (all channels)

```
npm bin/orchestra.js ─┐
Homebrew bin/orchestra ┼─> orchestra.sh "$@" ─> orchestra --ptyd-path <orchestra-ptyd> "$@"
tarball bin/orchestra ─┘     (exports ORCHESTRA_BIN = this orchestra.sh)
```

- `orchestra-mcp` and `orchestra-api` don't go through `orchestra.sh` (an MCP server's stdout
  carries protocol messages only): Homebrew and `install.sh` install `bin/` wrappers that export
  `ORCHESTRA_API_BIN`/`ORCHESTRA_PTYD_BIN` and `exec` the binary; npm's `bin/orchestra-mcp.js` /
  `bin/orchestra-api.js` run `install.js --invoke=…`, which spawns the binary from `dist/` with the
  same variables and sends its own messages to stderr. Without the variables, each binary looks
  for the others next to its own (symlink-resolved) path, then on `PATH`.
- Wrappers export `GW_TUI_BIN` and `GW_PTYD_BIN` as a pair (Homebrew uses `opt_libexec` paths, so
  they stay valid across upgrades; npm and `install.sh` use the install directory).
- `orchestra.sh` (`shell/gwr_binary.sh`) looks up the tui binary: `$GW_TUI_BIN`, `orchestra` next
  to the script, `tui/target/{release,debug}/tui`, `tui` in PATH. The daemon: `$GW_PTYD_BIN`,
  next to the tui binary, next to the script, PATH. Missing either is an error.
- `--help`, `--version` and `--check-updates` are handled before the daemon lookup; `--version`
  never starts a daemon (the Homebrew test relies on this).
- The terminal title and the daily background update check run only when no subcommand is given,
  so `$ORCHESTRA_BIN rename …` from hooks inside sessions stays quiet and fast.
- A caller-supplied `--ptyd-path` wins; `--services-path` is no longer passed (tui ignores it).
- Scripts that run on users' machines (`orchestra.sh`, `shell/gwr*`, `install.sh`) must work with
  macOS `/bin/bash` 3.2: no `${arr[@]}` on possibly empty arrays under `set -u`, no `${x^^}`,
  no `mapfile`.

## Directory structure

```
install/
├── AGENTS.md                      # This file
├── README.md                      # Short overview for maintainers
├── INSTALL.md                     # User installation guide
├── .gitignore                     # npm/THIRD_PARTY_NOTICES (generated)
├── licenses/
│   ├── about.toml                 # cargo-about config: accepted licenses, targets
│   └── THIRD_PARTY_NOTICES.hbs    # cargo-about template (plain text)
├── homebrew/
│   ├── orchestra.rb.template      # Formula source (edit this)
│   └── Formula/orchestra.rb       # Rendered formula = what the tap serves
├── npm/
│   ├── package.json, install.js, README.md, LICENSE (canonical license text)
│   ├── bin/orchestra.js, bin/orchestra-local.js
│   └── resources/, dist/, THIRD_PARTY_NOTICES   # generated, git-ignored
├── release-notes/<version>.md     # GitHub release notes for that version
└── scripts/
    ├── full-release-automation.sh # Whole release, in order
    ├── get-next-version.sh        # Next version from GitHub releases
    ├── build-release.sh           # Build binaries, packages, checksums, notes
    ├── generate-third-party-notices.sh
    ├── test-install.sh            # Pre-deployment checks of release/
    ├── create-github-release.sh   # gh release create + upload tarballs; notes from
    │                              # release-notes/<version>.md when it exists
    ├── generate-release-notes.sh  # Release notes from commit subjects (fallback)
    ├── release-homebrew.sh        # Render, verify (brew), push the formula
    ├── stage-npm-package.sh       # Fill install/npm from the tarballs
    ├── publish-npm.sh             # Stage, check packed files, npm publish
    ├── setup-public-repo.sh       # One-time public repo scaffold (README + LICENSE)
    └── check-github-auth.sh
```

## Release process

Run on macOS (it builds all three platforms):

```bash
./install/scripts/full-release-automation.sh              # next patch version
./install/scripts/full-release-automation.sh 0.6.0        # explicit version
./install/scripts/full-release-automation.sh --dry-run    # build, test, render, pack; publish nothing
```

Steps: `build-release.sh` → `test-install.sh` (`SKIP_BUILD=1`) → `create-github-release.sh` →
tracking tag `private-release/v<version>` → `release-homebrew.sh` → `publish-npm.sh`. Afterwards
commit `install/homebrew/Formula/orchestra.rb` (and `install/npm/package.json`).

Prerequisites: Rust with targets `x86_64-apple-darwin`, `aarch64-apple-darwin`,
`x86_64-unknown-linux-musl`; `cargo-zigbuild` + `zig` (Linux build);
`cargo install cargo-about --locked --features cli`; `gh auth login`; npm credentials
(`NPM_TOKEN`, `.env` or `npm login`); Homebrew, with `orchestra` not installed while
`release-homebrew.sh` verifies (it installs from a temporary tap).

### build-release.sh

`build-release.sh [VERSION] [INCREMENT] [--no-clean]`. Builds `-p tui -p orchestra-ptyd
-p orchestra-api -p orchestra-mcp` with
`--locked` and an explicit `--target` per platform (`ORCHESTRA_VERSION` is embedded by
`tui/build.rs`), checks each binary's architecture with `file`, generates
`release/THIRD_PARTY_NOTICES`, stages `release/orchestra-<platform>/`, and writes tarballs with
`COPYFILE_DISABLE=1`, `--no-mac-metadata --no-xattrs` (bsdtar) or `--no-xattrs` (GNU tar), uid/gid
0; it fails if a `._*` or `.DS_Store` entry remains. On Linux x86_64 it builds only `linux-x64`
(glibc), which is enough for local dry runs.

### Homebrew

`release-homebrew.sh <version> [--render-only|--no-verify|--no-push] [--output FILE]`:

1. Renders `orchestra.rb.template` with the version and the three SHA-256s from
   `release/checksums.txt` (each checked against its tarball) into `Formula/orchestra.rb`.
2. Verifies in a temporary tap `orchestra-release/verify`: `brew audit --strict --online`,
   `brew install`, `brew test`, uninstall, untap. Needs the GitHub release to be public.
3. Copies the formula into `$HOMEBREW_TAP_REPO` (default `../homebrew-orchestra`), commits
   `orchestra <version>`, pushes.

Formula rules:
- `license :cannot_represent` (a proprietary license has no SPDX id; `"Proprietary"` fails audit).
- URLs/SHAs live in `if OS.mac? && Hardware::CPU…` branches (taps may use OS conditionals; formula
  level `on_macos` blocks may not contain `url`). `version` is stated only in the arm64 and Linux
  branches: Homebrew parses `64` from `…-arm64.tar.gz`/`…-x64.tar.gz`, while for the Intel URL it
  parses `/v<version>/` and an explicit `version` fails audit as redundant.
- `libexec`: `orchestra`, `orchestra-ptyd`, `orchestra-api`, `orchestra-mcp`, `orchestra.sh`,
  `orchestra-local.sh`, `shell/`. `doc`: `LICENSE`, `THIRD_PARTY_NOTICES`. `bin/orchestra` =
  `write_env_script` to `opt_libexec/orchestra.sh` with `GW_TUI_BIN`/`GW_PTYD_BIN`;
  `bin/orchestra-mcp` and `bin/orchestra-api` = `write_env_script` to the binaries with
  `ORCHESTRA_API_BIN`/`ORCHESTRA_PTYD_BIN`.
- `test do`: `orchestra --version`, `orchestra-api --version` and `orchestra-mcp --version` match
  the formula version, `orchestra-ptyd --version` exits 0, license files exist. Never start a
  daemon or an API in the test.

### npm

`stage-npm-package.sh <version> [--allow-partial]` sets `package.json` `version`, extracts each
tarball into `resources/prebuilt/<platform>/{orchestra,orchestra-ptyd,orchestra-api,orchestra-mcp}`,
copies the launcher
(`orchestra.sh`, `orchestra-local.sh`, `shell/`) from one tarball into `resources/scripts/`, and
`release/THIRD_PARTY_NOTICES` into `install/npm/`. `publish-npm.sh <version> [--dry-run]` stages,
checks `npm pack --dry-run --json` for every required file and for removed ones, then publishes.

`install.js` (postinstall) copies the scripts and this platform's four binaries into `dist/`;
`bin/orchestra.js` runs `node install.js --invoke=orchestra <args>`, which execs
`dist/orchestra.sh` with `GW_TUI_BIN`/`GW_PTYD_BIN` set; `bin/orchestra-mcp.js` and
`bin/orchestra-api.js` spawn `dist/orchestra-mcp` / `dist/orchestra-api` directly. Only the first
argument is parsed as `--invoke`. Inside a source checkout it builds the binaries with cargo
instead.
`package.json` still lists `cpu: arm64` for Linux, but no linux-arm64 build exists; install.js
fails there with a clear message.

## Licensing

- Orchestra is proprietary (Antonio de Perio and Human Unsupervised Pty Ltd).
  `install/npm/LICENSE` is the one canonical copy: it matches the public repository's `LICENSE`
  and is what `setup-public-repo.sh`, the tarballs, the npm package and Homebrew ship. The in-app
  Help → License tab shows the same terms.
- npm: `"license": "SEE LICENSE IN LICENSE"`. Versions up to 0.5.81 were published with `MIT`
  metadata and can't be changed.
- `generate-third-party-notices.sh [OUTPUT]` runs `cargo about generate --workspace --locked`
  with `licenses/about.toml` and `licenses/THIRD_PARTY_NOTICES.hbs`: all crates linked into `tui`,
  `orchestra-ptyd`, `orchestra-api` and `orchestra-mcp` for the four release targets, without
  dev/build-only dependencies, with full license texts. It fails on a license outside `accepted`,
  on a third-party crate whose license can't be determined, and if `ratatui`, `portable-pty` or
  `rmcp` is missing (a binary dropped from the scan; `orchestra-api` has no crate of its own). Accepting a new license is a legal decision; copyleft beyond MPL-2.0's file-level
  terms must not be accepted. MPL-2.0 (`webpki-roots`) is used unmodified; the notices link its
  source.
- Herdr (Apache-2.0) and opencode-pty (MIT) are design references only. If code is ever copied,
  add its notice (and Apache `NOTICE` file) to the notices template.

## Testing without publishing

```bash
./install/scripts/build-release.sh 0.6.0 --no-clean        # Linux: linux-x64 only
SKIP_BUILD=1 ./install/scripts/test-install.sh 0.6.0
./install/scripts/publish-npm.sh 0.6.0 --dry-run           # changes install/npm/package.json
./install/scripts/release-homebrew.sh 0.6.0 --render-only --output /tmp/orchestra.rb   # needs all 3 tarballs
```

Manual checks before announcing a release (macOS arm64, macOS Intel, Linux x64 via Linuxbrew):
fresh `brew install humanunsupervised/orchestra/orchestra`, `brew upgrade` from the previous
version, `brew test orchestra`, `npm install -g @humanu/orchestra`, then `orchestra`,
`orchestra new --detach`, `orchestra ls`, `orchestra attach`, `orchestra daemon status`.
