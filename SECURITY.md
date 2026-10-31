# Security policy

## Reporting a vulnerability

Email **tony@humanunsupervised.com** with a description, affected version or commit, and steps to
reproduce. Do not publish exploits, credentials, pairing tokens, or session contents in a public
issue. Include only the minimum redacted logs needed to explain the problem.

Fixes target the latest development code and latest release. Older versions may require an
upgrade; there is no guaranteed response or maintenance timeframe.

## Trust boundaries

- Sessions run as your operating-system user. Session tokens prevent accidental misuse; they do
  not isolate hostile processes running as that user.
- The local API requires a bearer token and rejects browser origins. Remote access is opt-in
  and grants user-level access to anyone holding its pairing token.
- Use remote access only over a trusted private network such as Tailscale. Rotate the token
  with `rakumux remote token --rotate` after a device is lost or a token is exposed.
- Treat terminal output and session reports as untrusted data, including when read by agents.
- Releases are signed. Each release's `checksums.txt` carries a minisign (Ed25519) signature,
  `checksums.txt.minisig`, made on the maintainer's computer with a key that never lives in
  GitHub or CI. The in-app updater (`U` and automatic updates) refuses a release without a valid
  signature for that version by a key it carries (`install/release-keys.pub`), and so does
  `install.sh` when `minisign` or OpenSSL 3 is installed (otherwise it says the signature wasn't
  checked and relies on HTTPS and the checksums). So control of the GitHub repository or its
  workflows alone can't ship a release to installed Rakumux. Releases up to 0.1.40 were
  unsigned; updating from one of them trusts GitHub once.
- npm installs rely on npm's registry signatures and the package's provenance (trusted
  publishing from this repository's release workflow), and the release workflow publishes to
  npm only a release whose signature it has checked.

See [api/README.md](api/README.md) for listener authentication and identity rules.
