# Public release checklist

The public source repository is `humanunsupervised/rakumux`. The website is
<https://rakumux.com>. The proprietary mobile client belongs in the separate
`humanunsupervised/rakumux-mobile` repository.

The hosted account / AI / billing implementation is proprietary and belongs only in
private `humanunsupervised/rakumux-cloud`. Public `landing/` is static marketing;
the native app, local `rakumux-api` / MCP and canonical wire schemas remain public.
Do not nest the cloud repository here, add it as a submodule, or copy its application
code into public branches or source/package archives. Ignore rules do not establish
privacy. Previously public Apache-2.0 marketing/contracts retain their license.

## Before publishing source

- [ ] Run `node tests/public-source-boundary.mjs` and its regression tests. Check
  every ref / source archive independently: this guard validates the selected tree,
  not the entire Git history or all possible secrets. Cloud code that was already
  published requires a separately reviewed exposure/history response; deletion alone
  does not make earlier copies private.
- [ ] Confirm ownership and permission to release the core under Apache-2.0.
- [ ] Choose a clean public snapshot or a separately reviewed, filtered history.
  Removing `mobile/` from the working tree does **not** remove it from earlier commits, tags,
  branches, or source archives. Do not publish the existing history while it contains private
  mobile source or other material that must remain private.
- [ ] Scan the intended public tree and its entire intended history for credentials, personal
  data, private endpoints, and material you cannot redistribute. Rotate any exposed secrets.
- [ ] Review all refs that would be pushed. Do not use `git push --mirror` for a private-to-public
  migration. History rewriting and remote publication require explicit maintainer approval.
- [ ] Run the checks in [CONTRIBUTING.md](CONTRIBUTING.md).
- [ ] Configure repository permissions, branch protection, and private vulnerability reporting.

## Before publishing packages

- [ ] Build fresh Rakumux binaries and tarballs. Do not reuse packages or checksums from a
  differently named release.
- [ ] Generate `THIRD_PARTY_NOTICES` and run the isolated package-install checks.
- [ ] Create the release against the exact reviewed source commit; package scripts verify its
  `v<version>` tag rather than letting GitHub select a default-branch commit.
- [ ] For marketing-only deployment, verify hosting and DNS, then deploy static `landing/`.

`landing/` is the public static marketing deployment only. The account-capable
website deploys from `website/` in private `rakumux-cloud`, after separate provider,
hosting and DNS approval. Public builds/releases must not fetch private cloud source
or credentials. Never publish private cloud source as part of this repository's release.

The current production entry point is `./release.sh`. It publishes through GitHub Actions only
after every selected platform and isolated package-install check succeeds: first the GitHub
release that `curl -fsSL https://rakumux.com/install.sh | bash` installs, then the same packages
to npm as `rakumux` (a prerelease under the `next` dist-tag; not for a draft, nor without all
three targets). Select one target or a comma-separated list with `--targets`; without an
override, macOS Intel, macOS Apple Silicon and Linux x64 are all built on native GitHub-hosted
runners.

### npm (every release)

- [ ] Once: the npm organization `rakumux` owns the `rakumux` package, and the package has a
  trusted publisher on npmjs.com (Settings → Trusted Publisher → GitHub Actions: organization or
  user `humanunsupervised`, repository `rakumux`, workflow filename `release.yml`, no
  environment). No npm token is stored in GitHub.
- [ ] Keep `install/npm/package.json`'s `repository.url` on this repository; npm's provenance
  checks it.
- [ ] After each release, check the workflow's `npm` job. If it failed, the GitHub release is
  already public and only npm lags: fix the cause and re-run the job (a version already on npm
  is skipped), or publish by hand with
  `./install/scripts/publish-npm.sh <version> --from-release`. `--dry-run` shows what npm would
  get, without credentials.

### Homebrew (optional, not part of the release)

- [ ] Confirm control of the `humanunsupervised/homebrew-rakumux` tap. It is configured, not
  provisioned by this checkout.
- [ ] Render and verify the Homebrew formula from those fresh tarball checksums.

See [install/AGENTS.md](install/AGENTS.md) for release commands. Preparing this checkout does not
publish packages, push source, change repository visibility, or modify DNS.
