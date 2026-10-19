# Public release checklist

The public source repository is `humanunsupervised/rakumux`. The website is
<https://rakumux.com>. The proprietary mobile client belongs in the separate
`humanunsupervised/rakumux-mobile` repository.

## Before publishing source

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
- [ ] Verify `rakumux.com` hosting and DNS, then deploy the website from `landing/`.

The current production entry point is `./release.sh`: binary releases only, installed with
`curl -fsSL https://rakumux.com/install.sh | bash`. It publishes through GitHub Actions only
after every platform and isolated package-install check succeeds.

### Optional registries (not part of the binary release)

- [ ] Confirm control of npm `rakumux` and the
  `humanunsupervised/homebrew-rakumux` tap. These names are configured, not provisioned by this
  checkout.
- [ ] Render and verify the Homebrew formula from those fresh tarball checksums.
- [ ] Stage npm from those tarballs and verify the packed license and contents.

See [install/AGENTS.md](install/AGENTS.md) for release commands. Preparing this checkout does not
publish packages, push source, change repository visibility, or modify DNS.
