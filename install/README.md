# install/

Production distribution is native release tarballs on GitHub, installed with the curl command
below, and the same packages on npm as `rakumux`. They ship four binaries (`rakumux`,
`rakumux-ptyd`, `rakumux-api`, `rakumux-mcp`), the `rakumux.sh` launcher, `LICENSE` and
`THIRD_PARTY_NOTICES`. Every release publishes both; Homebrew helpers remain for future channel
setup.

## Users

```bash
curl -fsSL https://rakumux.com/install.sh | bash
npm install -g rakumux        # or with npm
```

See [INSTALL.md](INSTALL.md) for every option and troubleshooting.

## Maintainers

```bash
./release.sh                               # macOS Intel + Apple Silicon + Linux x64
./release.sh --targets=linux-x64            # Linux-only override
./release.sh --dry-run                     # validate all targets; publish nothing
```

GitHub Actions publishes the GitHub release, then npm (trusted publishing; one-time setup and
re-runs in [AGENTS.md](AGENTS.md#npm)).

| Task | Script |
|---|---|
| Build packages | `scripts/build-release.sh <version> [--no-clean]` |
| Check packages | `SKIP_BUILD=1 scripts/test-install.sh <version>` |
| Third-party notices | `scripts/generate-third-party-notices.sh [output]` |
| GitHub release | `scripts/create-github-release.sh <version>` |
| Homebrew formula | `scripts/release-homebrew.sh <version>` (edit `homebrew/rakumux.rb.template`) |
| npm | `scripts/publish-npm.sh <version> --from-release [--dry-run]` |

Details (package layout, launcher lookup, formula rules, licensing policy):
[AGENTS.md](AGENTS.md).
