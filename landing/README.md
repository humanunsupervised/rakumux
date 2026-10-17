# landing/

Rakumux's marketing site: one page that says what Rakumux is, why developers would use it and
how to install it. A standalone Next.js app (not part of the Rust workspace) built as a static
export.

```bash
npm install
npm run dev        # http://localhost:3000
npm run build      # static site in out/
npm run typecheck
```

## Deploying to Vercel

The Vercel project is `rakumux` in the `adeperios-projects` team, served at
<https://rakumux.com> (and `www.rakumux.com`). Deploy from this folder with the Vercel CLI:

```bash
vercel link --yes --project rakumux --scope adeperios-projects   # once per clone
vercel deploy --prod
```

`vercel.json` sets the Next.js preset (a project created from the CLI has none and looks for
`public/`); `next.config.ts` sets `output: "export"`, so Vercel serves the prerendered files and
runs no server code. `.vercelignore` keeps `node_modules`, build output and `.env*` out of the
upload. The site needs no environment variables. To deploy from git instead, connect the
repository in Vercel and set **Root Directory** to `landing`.

Any static host works too: serve `out/` after `npm run build`.

## Layout

- `app/page.tsx`: the sections (hero, why, modes, agents, keys, install).
- `app/globals.css`: the whole design. Tokyo Night colours from the TUI, with the RAKUMUX
  badge's orange (`#ff9e64`) as the accent; Geist Sans and Geist Mono from the `geist` package, so
  the build makes no font requests.
- `components/TuiDemo.tsx`: a working copy of the TUI's screen in the hero. It plays by itself
  until clicked, then takes `j`/`k`, `gg`/`G`, `Enter`, `Esc`, `1`-`5` and `Ctrl-\`. Tab is left
  to the browser so keyboard users can leave it; autoplay is off under reduced motion.
- `components/InstallTabs.tsx`, `components/CopyCommand.tsx`: install commands with copy buttons.

Install commands and links follow `install/INSTALL.md` (npm `rakumux`, the
`humanunsupervised/rakumux` Homebrew tap and GitHub releases); change them together. The copy
describes Rakumux 0.6 (sessions, two modes, MCP), so publish it with that release.
