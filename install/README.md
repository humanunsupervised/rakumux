# install/

Distribution of Rakumux: release tarballs on GitHub, the Homebrew tap and the npm package. All
three ship the same four binaries (`rakumux`, `rakumux-ptyd`, `rakumux-api`, `rakumux-mcp`), the `rakumux.sh` launcher,
`LICENSE` and `THIRD_PARTY_NOTICES`, and all three run `rakumux.sh` for every argument.

## Users

```bash
npm install -g rakumux
# or
brew install humanunsupervised/rakumux/rakumux
```

See [INSTALL.md](INSTALL.md) for every option, upgrading from 0.5.x and troubleshooting.

## Maintainers

```bash
./install/scripts/full-release-automation.sh            # release the next patch version (macOS)
./install/scripts/full-release-automation.sh --dry-run  # build, test, render, pack; publish nothing
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
