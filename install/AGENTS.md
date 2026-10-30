# install/ - Open-source distribution of Rakumux

Rakumux ships as four native binaries plus a small bash launcher, through three channels (npm,
Homebrew, release tarballs) that all behave the same way. Every release publishes the tarballs
(GitHub) and npm; Homebrew isn't published. The source and releases live together
in `humanunsupervised/rakumux`. The website is <https://rakumux.com>.

## Repositories

| Repository | Visibility | Contents |
|---|---|---|
| `humanunsupervised/rakumux` | public | Source, docs, build scripts, Apache-2.0 `LICENSE`, GitHub Releases |
| `humanunsupervised/rakumux-mobile` | separate, proprietary | Mobile app; never stage its source here |
| `humanunsupervised/homebrew-rakumux` | public | `Formula/rakumux.rb` (the tap) |
| npm `rakumux` | public | Owned by the npm organization `rakumux`; every release publishes the GitHub release's tarballs to it |

## What ships

Every release tarball `rakumux-<platform>.tar.gz` (`macos-arm64`, `macos-intel`, `linux-x64`)
contains `rakumux-<platform>/`:

| Path | Purpose |
|---|---|
| `rakumux` | The `tui` binary: TUI with no command, CLI subcommands otherwise |
| `rakumux-ptyd` | Session daemon (Cargo workspace member `tui/crates/ptyd`), spawned on demand by clients |
| `rakumux-api` | External HTTP API on 127.0.0.1 (workspace member `api/`), started on demand by `rakumux-mcp` |
| `rakumux-mcp` | MCP server (stdio) for coding agents (workspace member `mcp/`) |
| `rakumux.sh` | Launcher every channel runs, for every argument |
| `shell/gwr_*.sh`, `shell/gwr/check-updates.sh` | Launcher modules |
| `shell/rakumux-command-hook.sh` | Command-history hook for sessions (AI naming input) |
| `rakumux-local.sh` | Launches a local source checkout (development) |
| `LICENSE` | Apache License 2.0, from the repository root `LICENSE` |
| `THIRD_PARTY_NOTICES` | Licenses of every crate linked into the binaries |
| `install.sh`, `README.md` | Tarball installer and package readme |

Runtime dependencies: bash only. No git, tmux, jq, python3 or services layer; `env-copy` and
`rakumux-cli` were removed in 0.6.0 and must not ship again (`test-install.sh` and
`publish-npm.sh` fail if they do).

## Launch path (all channels)

```
npm bin/rakumux.js ─┐
Homebrew bin/rakumux ┼─> rakumux.sh "$@" ─> rakumux --ptyd-path <rakumux-ptyd> "$@"
tarball bin/rakumux ─┘     (exports RAKUMUX_BIN = this rakumux.sh)
```

- `rakumux-mcp` and `rakumux-api` don't go through `rakumux.sh` (an MCP server's stdout
  carries protocol messages only): Homebrew and `install.sh` install `bin/` wrappers that export
  `RAKUMUX_API_BIN`/`RAKUMUX_PTYD_BIN` and `exec` the binary; npm's `bin/rakumux-mcp.js` /
  `bin/rakumux-api.js` run `install.js --invoke=…`, which spawns the binary from `dist/` with the
  same variables and sends its own messages to stderr. Without the variables, each binary looks
  for the others next to its own (symlink-resolved) path, then on `PATH`.
- Wrappers export `GW_TUI_BIN` and `GW_PTYD_BIN` as a pair (Homebrew uses `opt_libexec` paths, so
  they stay valid across upgrades; npm and `install.sh` use the install directory).
- `rakumux.sh` (`shell/gwr_binary.sh`) looks up the tui binary: `$GW_TUI_BIN`, `rakumux` next
  to the script, `tui/target/{release,debug}/tui`, `tui` in PATH. The daemon: `$GW_PTYD_BIN`,
  next to the tui binary, next to the script, PATH. Missing either is an error.
- `--help`, `--version` and `--check-updates` are handled before the daemon lookup; `--version`
  never starts a daemon (the Homebrew test relies on this).
- The terminal title and the daily background update check run only when no subcommand is given,
  so `$RAKUMUX_BIN rename …` from hooks inside sessions stays quiet and fast.
- A caller-supplied `--ptyd-path` wins; `--services-path` is no longer passed (tui ignores it).
- Scripts that run on users' machines (`rakumux.sh`, `shell/gwr*`, `install.sh`) must work with
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
│   ├── rakumux.rb.template      # Formula source (edit this)
│   └── Formula/rakumux.rb       # Generated and ignored; what the tap serves
├── npm/
│   ├── package.json, install.js, README.md
│   ├── bin/rakumux.js, bin/rakumux-local.js
│   └── resources/, dist/, LICENSE, THIRD_PARTY_NOTICES   # generated, git-ignored
├── release-notes/<version>.md     # GitHub release notes for that version
└── scripts/
    ├── get-next-version.sh        # Next version from GitHub releases
    ├── build-release.sh           # Build binaries, packages, checksums, notes
    ├── generate-third-party-notices.sh
    ├── test-install.sh            # Pre-deployment checks of release/
    ├── create-github-release.sh   # gh release create + upload tarballs; notes from
    │                              # release-notes/<version>.md when it exists
    ├── generate-release-notes.sh  # Release notes from commit subjects (fallback)
    ├── release-homebrew.sh        # Render, verify (brew), push the formula
    ├── stage-npm-package.sh       # Fill install/npm from the tarballs
    ├── publish-npm.sh             # Stage (a GitHub release, or release/), check packed files, npm publish
    └── check-github-auth.sh
```

## Release process

The entry point is `release.sh` at the repository root. Run from a clean,
reviewed public checkout after pushing its source commit to `main`:

```bash
./release.sh                               # next patch, all three native targets
./release.sh --draft                       # build all three; leave release unpublished
./release.sh --targets=linux-x64            # explicit Linux-only override
./release.sh --targets=linux-x64 --dry-run  # validate; create/push nothing
```

The script verifies that HEAD equals public `main`, then creates and pushes an annotated
`v<version>` tag carrying the requested targets. `.github/workflows/release.yml` validates that
selection and builds only those targets on native runners. Supported selections are `linux-x64`
(static musl), `macos-arm64` and `macos-intel`. All three are selected by default, independent
of the invoking machine. `--targets` overrides that selection.
Every selected package passes `test-install.sh` with an isolated install prefix. The publish
job verifies the complete selected asset set and combines its checksums before uploading a draft,
then makes the complete release
public. `--draft` leaves it unpublished; `--prerelease` does not replace the latest stable release.
The public installer downloads these GitHub release assets. Then the `npm` job publishes the same
assets to npm (see [npm](#npm)): every release with all three targets that isn't a draft, a
prerelease under the `next` dist-tag. Homebrew isn't published.
`v0.1.3` publishes all three platforms after their native checks and isolated package installs pass.

Local prerequisites: git push access to the public repository; curl for automatic version
selection. No local GitHub CLI, npm or Homebrew login is needed. Actions uses its scoped
`GITHUB_TOKEN`, and npm trusts the workflow (trusted publishing, no npm secret). For manual package builds, install Rust and cargo-about; cross-building Linux
from macOS also needs cargo-zigbuild and zig. Follow `PUBLISHING.md` before publishing source.

### build-release.sh

`build-release.sh [VERSION] [INCREMENT] [--no-clean] [--platform=PLATFORM]`. Builds `-p tui -p rakumux-ptyd
-p rakumux-api -p rakumux-mcp` with
`--locked` and an explicit `--target` per platform (`RAKUMUX_VERSION` is embedded by
`tui/build.rs`), checks each binary's architecture with `file`, generates
`release/THIRD_PARTY_NOTICES`, stages `release/rakumux-<platform>/`, and writes tarballs with
`COPYFILE_DISABLE=1`, `--no-mac-metadata --no-xattrs` (bsdtar) or `--no-xattrs` (GNU tar), uid/gid
0; it fails if a `._*` or `.DS_Store` entry remains. On Linux x86_64 it builds only `linux-x64`
(glibc), which is enough for local dry runs.

### Homebrew

`release-homebrew.sh <version> [--render-only|--no-verify|--no-push] [--output FILE]`:

1. Renders `rakumux.rb.template` with the version and the three SHA-256s from
   `release/checksums.txt` (each checked against its tarball) into `Formula/rakumux.rb`.
2. Verifies in a temporary tap `rakumux-release/verify`: `brew audit --strict --online`,
   `brew install`, `brew test`, uninstall, untap. Needs the GitHub release to be public.
3. Copies the formula into `$HOMEBREW_TAP_REPO` (default `../homebrew-rakumux`), commits
   `rakumux <version>`, pushes.

Formula rules:
- `license "Apache-2.0"`.
- URLs/SHAs live in `if OS.mac? && Hardware::CPU…` branches (taps may use OS conditionals; formula
  level `on_macos` blocks may not contain `url`). `version` is stated only in the arm64 and Linux
  branches: Homebrew parses `64` from `…-arm64.tar.gz`/`…-x64.tar.gz`, while for the Intel URL it
  parses `/v<version>/` and an explicit `version` fails audit as redundant.
- `libexec`: `rakumux`, `rakumux-ptyd`, `rakumux-api`, `rakumux-mcp`, `rakumux.sh`,
  `rakumux-local.sh`, `shell/`. `doc`: `LICENSE`, `THIRD_PARTY_NOTICES`. `bin/rakumux` =
  `write_env_script` to `opt_libexec/rakumux.sh` with `GW_TUI_BIN`/`GW_PTYD_BIN`;
  `bin/rakumux-mcp` and `bin/rakumux-api` = `write_env_script` to the binaries with
  `RAKUMUX_API_BIN`/`RAKUMUX_PTYD_BIN`.
- `test do`: `rakumux --version`, `rakumux-api --version` and `rakumux-mcp --version` match
  the formula version, `rakumux-ptyd --version` exits 0, license files exist. Never start a
  daemon or an API in the test.

### npm

Every release goes to npm as `rakumux`, made of the packages `install.sh` installs: after
`publish` makes the GitHub release public, `release.yml`'s `npm` job (`contents: read`,
`id-token: write`; Node 24 with `npm@11.21.1`) runs
`publish-npm.sh <version> --from-release --tag=<latest|next>` from the tag's source. One path for
CI and by hand:

- `publish-npm.sh <version> [--from-release] [--tag=TAG] [--dry-run]`. `--from-release` reads the
  published GitHub release `v<version>` (a draft, or one without all three tarballs, is refused),
  downloads its tarballs and `checksums.txt` into a temporary directory, checks each tarball's
  SHA-256, takes `THIRD_PARTY_NOTICES` from `rakumux-linux-x64.tar.gz`, writes `RELEASE_NOTES.md`
  (`# Rakumux v<version>`, then `release-notes/<version>.md` after its first line) and stages from
  there. Without it, it stages a local build in `release/`. Then it checks
  `npm pack --dry-run --json` for every required file and for removed ones, and runs
  `npm publish --access public --tag <tag>` in `install/npm`.
- Dist-tags: `latest`, or `next` for a prerelease (a version with a `-`, or a GitHub release marked
  as one); a prerelease never goes to `latest`. The updater and `--check-updates` read `latest`.
- A version already on npm is left alone (exit 0, a notice in the run), so re-runs are safe.
- `--dry-run` needs no credentials and publishes nothing; with `--from-release` it shows exactly
  what npm would get. `GH_TOKEN`/`GITHUB_TOKEN`, when set, reads the release's metadata (handed to
  curl on stdin, never printed).
- Auth: in GitHub Actions with `id-token: write`, **trusted publishing** (OIDC): npm trades the
  job's token for a short-lived publish token and adds provenance; no npm token is stored anywhere.
  It needs npm 11.5.1 or newer, and `package.json`'s `repository.url` must stay this repository.
  By hand: an `npm login` session with publish rights in the `rakumux` organization (or
  `NPM_TOKEN`, and `NPM_OTP`).
- The job skips npm, with a notice or warning in the run, for a `--draft` release and for one
  without all three targets (the npm package carries every platform).

**One-time setup** (after the first version, 0.1.33, was published by hand to create the
package): on npmjs.com, package `rakumux` → Settings → Trusted Publisher → GitHub Actions:
organization or user `humanunsupervised`, repository `rakumux`, workflow filename `release.yml`,
environment name empty. npm then recommends Publishing access → "Require two-factor
authentication and disallow tokens"; trusted publishing keeps working.

**When the npm job fails**, the GitHub release is already public and `install.sh` serves it; only
npm lags. Fix the cause (most often the trusted publisher's fields) and re-run the failed `npm` job
from the workflow run. By hand instead, from a checkout of the tag and logged in to npm:
`./install/scripts/publish-npm.sh <version> --from-release`. A draft made public later on GitHub
goes to npm the same way.

`stage-npm-package.sh <version> [--allow-partial] [--release-dir=DIR]` sets `package.json`
`version`, extracts each tarball into
`resources/prebuilt/<platform>/{rakumux,rakumux-ptyd,rakumux-api,rakumux-mcp}`, copies the root
`LICENSE` and the launcher (`rakumux.sh`, `rakumux-local.sh`, `shell/`) from one tarball into
`resources/scripts/`, and `THIRD_PARTY_NOTICES` into `install/npm/`, reading from `release/` or
`DIR`.

`install.js` (postinstall) copies the scripts and this platform's four binaries into `dist/`;
`bin/rakumux.js` runs `node install.js --invoke=rakumux <args>`, which execs
`dist/rakumux.sh` with `GW_TUI_BIN`/`GW_PTYD_BIN` set; `bin/rakumux-mcp.js` and
`bin/rakumux-api.js` spawn `dist/rakumux-mcp` / `dist/rakumux-api` directly. Only the first
argument is parsed as `--invoke`. Inside a source checkout it builds the binaries with cargo
instead.
`package.json` still lists `cpu: arm64` for Linux, but no linux-arm64 build exists; install.js
fails there with a clear message.

## Licensing

- Rakumux core is Apache-2.0 (Antonio de Perio and Human Unsupervised Pty Ltd).
  The root `LICENSE` is canonical. The tarballs, staged npm package, Homebrew, and in-app
  Help → License tab use this text. The mobile app is separate and is not covered by this grant.
- npm and every Cargo workspace member declare `Apache-2.0`.
- `generate-third-party-notices.sh [OUTPUT]` runs `cargo about generate --workspace --locked`
  with `licenses/about.toml` and `licenses/THIRD_PARTY_NOTICES.hbs`: all crates linked into `tui`,
  `rakumux-ptyd`, `rakumux-api` and `rakumux-mcp` for the four release targets, without
  dev/build-only dependencies, with full license texts. It fails on a license outside `accepted`,
  on a third-party crate whose license can't be determined, and if `ratatui`, `portable-pty` or
  `rmcp` is missing (a binary dropped from the scan; `rakumux-api` has no crate of its own). Accepting a new license is a legal decision; copyleft beyond MPL-2.0's file-level
  terms must not be accepted. MPL-2.0 (`webpki-roots`) is used unmodified; the notices link its
  source.
- Herdr (Apache-2.0) and opencode-pty (MIT) are design references only. If code is ever copied,
  add its notice (and Apache `NOTICE` file) to the notices template.

## Testing without publishing

```bash
./install/scripts/build-release.sh 0.1.0 --no-clean        # Linux: linux-x64 only
SKIP_BUILD=1 ./install/scripts/test-install.sh 0.1.0
./install/scripts/publish-npm.sh 0.1.0 --dry-run           # changes install/npm/package.json
./install/scripts/publish-npm.sh 0.1.33 --from-release --dry-run   # what npm gets from that release
bash tests/test-npm-release.sh                             # offline: fixture releases, stub npm
./install/scripts/release-homebrew.sh 0.1.0 --render-only --output /tmp/rakumux.rb   # needs all 3 tarballs
```

Manual checks before announcing a release (macOS arm64, macOS Intel, Linux x64 via Linuxbrew):
fresh `brew install humanunsupervised/rakumux/rakumux`, `brew upgrade` from the previous
version, `brew test rakumux`, `npm install -g rakumux`, then `rakumux`,
`rakumux new --detach`, `rakumux ls`, `rakumux attach`, `rakumux daemon status`.
