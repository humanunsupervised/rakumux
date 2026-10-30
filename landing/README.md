# landing/

Rakumux's marketing site: one page that says what Rakumux is, why developers would use it and
how to install it. A standalone Next.js static export, separate from the Rust workspace.
This public project contains no hosted account, AI or billing implementation.

```bash
npm install
npm run dev        # http://localhost:3000
npm run build      # static site in out/
npm run typecheck
```

## Deploying to Vercel

The production domain is <https://rakumux.com>. Link this folder to the intended Vercel project
under an account you control, then configure that domain in Vercel and your DNS provider.
Deploy from this folder with the Vercel CLI:

```bash
vercel link            # select your team and project; once per clone
vercel deploy --prod
```

`vercel.json` sets the Next.js preset (a project created from the CLI has none and looks for
`public/`); `next.config.ts` sets `output: "export"`. No Clerk configuration is required.
`.vercelignore` keeps dependencies, build output and `.env*` out of uploads.
To deploy from git, connect the
repository in Vercel and set **Root Directory** to `landing`.

Any static host can serve `out/` after `npm run build`.

## Private hosted application

The account-capable website lives in private `humanunsupervised/rakumux-cloud`,
under `website/`. It owns Clerk pages and future AI/billing server implementation.
This public folder stays independently buildable with no private dependency or keys.
The private deployment includes a licensed marketing snapshot so it can preserve
the proposed rakumux.com account paths without moving private code into this repository.

Public marketing source remains Apache-2.0. New cloud-specific source is proprietary;
moving already public marketing code does not revoke its license. Do not copy private
account routes, SDK configuration or server handlers back here. The source-boundary
check in `tests/public-source-boundary.mjs` enforces the public tree's intended shape.
It is not a complete secret or Git-history scan. See [PUBLISHING.md](../PUBLISHING.md).

No production deployment, DNS change or provider provisioning is performed by this split.

## Layout

- `app/page.tsx`: the sections (hero, why, modes, agents, keys, install).
- `app/globals.css`: the whole design. Tokyo Night colours from the TUI, with the RAKUMUX
  badge's orange (`#ff9e64`) as the accent; Geist Sans and Geist Mono from the `geist` package, so
  the build makes no font requests.
- `components/TuiDemo.tsx`: a working copy of the TUI's screen in the hero. It plays by itself
  until clicked, then takes `j`/`k`, `gg`/`G`, `Enter`, `Esc`, `1`-`5` and `Ctrl-\`. Tab is left
  to the browser so keyboard users can leave it; autoplay is off under reduced motion.
  Descriptions put the colour-coded app first, then its branch and state. App colours and status
  marks match `tui/src/components/format.rs`; previews stay full-colour in both modes.
- `components/CopyCommand.tsx`: install commands with copy buttons.
- `public/install.sh`: checksum-verifying installer served at <https://rakumux.com/install.sh>.

Install commands and links follow `install/INSTALL.md`. The website promotes the curl installer;
every release also goes to npm as `rakumux`, and maintainer packaging supports the
`humanunsupervised/rakumux` Homebrew tap.
Keep the installer, release links, and documentation consistent.
