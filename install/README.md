# install/

Production distribution is native release tarballs on GitHub, installed with the curl command
below. They ship four binaries (`rakumux`, `rakumux-ptyd`, `rakumux-api`, `rakumux-mcp`), the
`rakumux.sh` launcher, `LICENSE` and `THIRD_PARTY_NOTICES`. npm and Homebrew helpers remain for
future channel setup; those registries are not part of the current production release.

## Users

```bash
curl -fsSL https://rakumux.com/install.sh | bash
```

See [INSTALL.md](INSTALL.md) for every option and troubleshooting.

## Maintainers

```bash
./release.sh --targets=linux-x64             # next Linux binary release through Actions
./release.sh 0.1.1 --targets=linux-x64        # explicit Linux-only release
./release.sh --targets=linux-x64 --dry-run  # validate and preview; publish nothing
```

| Task | Script |
|---|---|
| Build packages | `scripts/build-release.sh <version> [--no-clean]` |
| Check packages | `SKIP_BUILD=1 scripts/test-install.sh <version>` |
| Third-party notices | `scripts/generate-third-party-notices.sh [output]` |
| GitHub release | `scripts/create-github-release.sh <version>` |
| Homebrew formula | `scripts/release-homebrew.sh <version>` (edit `homebrew/rakumux.rb.template`) |
| npm | `scripts/publish-npm.sh <version> [--dry-run]` |

Details (package layout, launcher lookup, formula rules, licensing policy):
[AGENTS.md](AGENTS.md).
