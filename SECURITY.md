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

See [api/README.md](api/README.md) for listener authentication and identity rules.
