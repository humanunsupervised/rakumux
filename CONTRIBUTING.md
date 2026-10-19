# Contributing to Rakumux

Open an [issue](https://github.com/humanunsupervised/rakumux/issues) for a bug or proposal.
For substantial changes, agree on scope before implementing them. Report vulnerabilities using
[SECURITY.md](SECURITY.md), not public issues.

## Setup

Install stable Rust, a C compiler, and bash. Run `./build.sh --debug`, then `./rakumux.sh`.
For an isolated development instance, set `RAKUMUX_HOME` to a temporary directory before launching.
Never run tests against your real `~/.rakumux`.

Read [AGENTS.md](AGENTS.md) and the directory-specific `AGENTS.md` files before changing code.
The daemon owns sessions and policy; the UI, CLI, and HTTP API are clients. The product never
runs Git or tmux. Keep wire-contract changes additive.

## Checks

From `tui/`:

```bash
cargo build --workspace --locked
cargo test --workspace --locked
cargo fmt --all --check
```

From the repository root:

```bash
bash tests/test-repository.sh
bash tests/test-command-hook.sh
bash tests/test-release.sh
bash tests/test-release-tag.sh
```

For website changes, run `npm ci`, `npm run typecheck`, and `npm run build` in `landing/`.
Node 22 or later is recommended. Release tooling has additional requirements in
[install/AGENTS.md](install/AGENTS.md).
Default CI covers Linux, the currently published binary platform. A release selecting macOS
targets runs their native checks before those packages can be published.

## Pull requests

- Keep each change focused and include regression tests.
- Explain the behavior change and list checks you ran.
- Update affected documentation, API contracts, and packaging together.
- Do not commit credentials, local session data, generated packages, or mobile source.

Unless explicitly stated otherwise, contributions are submitted under the repository's
[Apache-2.0 license](LICENSE). This repository does not require copyright assignment.
