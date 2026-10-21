import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { checkPublicSourceBoundary } from "./public-source-boundary.mjs";

const guardPath = fileURLToPath(new URL("./public-source-boundary.mjs", import.meta.url));
const temporaryRoot = existsSync("/tmp/opencode") ? "/tmp/opencode" : tmpdir();

function isolatedEnvironment(root) {
  return { PATH: process.env.PATH, HOME: root, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", LC_ALL: "C" };
}

function write(root, path, contents) {
  mkdirSync(dirname(join(root, path)), { recursive: true });
  writeFileSync(join(root, path), contents);
}

function git(root, ...args) {
  return execFileSync("git", ["-c", "core.fsmonitor=false", "-C", root, ...args], {
    encoding: "utf8",
    env: isolatedEnvironment(root),
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function repository(context) {
  const root = mkdtempSync(join(temporaryRoot, "rakumux-public-boundary-"));
  context.after(() => rmSync(root, { recursive: true, force: true }));
  git(root, "init", "--quiet");
  write(root, "landing/next.config.ts", 'const config = { output: "export", images: { unoptimized: true } };\nexport default config;\n');
  write(root, "landing/package.json", JSON.stringify({ private: true, dependencies: { next: "16.3.8" } }));
  write(root, "landing/package-lock.json", JSON.stringify({ lockfileVersion: 3, packages: {} }));
  write(root, "landing/app/page.tsx", 'export default function Page() { return <a href="https://rakumux.com">Rakumux</a>; }\n');
  git(root, "add", "--", "landing");
  return root;
}

function commit(root) {
  // Per-command identity only: never write global or repository user configuration.
  git(root, "-c", "user.name=Boundary Test", "-c", "user.email=boundary@example.invalid",
    "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", "commit", "--quiet", "-m", "Synthetic public source");
}

function hasViolation(root, path, reason, snapshot = "working tree") {
  const violations = checkPublicSourceBoundary(root);
  assert.ok(violations.some((entry) => entry.startsWith(`${snapshot}: ${JSON.stringify(path)}:`) && entry.includes(reason)),
    `Expected ${snapshot} violation for ${path}: ${violations.join("\n")}`);
  return violations;
}

test("static marketing, local clients/auth and public contracts remain public", (context) => {
  const root = repository(context);
  for (const path of ["api/src/main.rs", "mcp/src/main.rs", "tui/src/auth/clerk.rs", "auth/AGENTS.md",
    "contracts/session-status/request.schema.json", "contracts/session-status/fixtures/request.json"]) {
    write(root, path, "Synthetic public local source or wire contract.\n");
  }
  write(root, "README.md", "Cloud implementation belongs in humanunsupervised/rakumux-cloud, not cloud/.\n");
  write(root, "landing/README.md", "Account routes and @clerk/nextjs belong in the private rakumux-cloud repository.\n");
  git(root, "add", "--", ".");
  commit(root);
  assert.deepEqual(checkPublicSourceBoundary(root), []);
});

for (const name of ["cloud", "rakumux-cloud"]) {
  test(`rejects ignored ${name}/, including an empty directory`, (context) => {
    const root = repository(context);
    write(root, ".gitignore", `${name}/\n`);
    mkdirSync(join(root, name));
    git(root, "check-ignore", "--quiet", "--", `${name}/`);
    hasViolation(root, name, "private cloud source");
  });

  test(`rejects tracked ${name}/ even after staged removal from the working tree`, (context) => {
    const root = repository(context);
    write(root, `${name}/server.ts`, "Synthetic private implementation.\n");
    git(root, "add", "--", name);
    commit(root);
    git(root, "rm", "--quiet", "--", `${name}/server.ts`);
    hasViolation(root, `${name}/server.ts`, "private cloud source", "HEAD");
  });
}

test("rejects a private-root symlink without following it", (context) => {
  const root = repository(context);
  symlinkSync("missing-private-source", join(root, "cloud"));
  hasViolation(root, "cloud", "private cloud source");
});

for (const path of [
  "landing/app/(account)/layout.tsx",
  "landing/app/(public)/sign-in/[[...sign-in]]/page.tsx",
  "landing/src/app/sign-up/[[...sign-up]]/page.tsx",
  "landing/app/connect/page.tsx",
  "landing/app/welcome/page.tsx",
  "landing/app/account/billing/page.tsx",
  "landing/src/pages/account.tsx",
  "landing/lib/account/continuation.ts",
  "landing/components/account/AccountPage.tsx",
  "landing/middleware.ts",
  "landing/src/proxy.ts",
  "landing/pages/private/_middleware.js",
  "landing/app/api/v1/ai/session-status/route.ts",
  "landing/src/app/(server)/api/billing/portal/route.ts",
  "landing/pages/api/stripe/webhook.js",
  "api/next.config.ts",
  "api/src/proxy.ts",
  "api/app/api/v1/ai/session-status/route.ts",
  "api/src/pages/api/session-status.ts",
  "api/src/session-status/service.ts",
  "api/src/production.ts",
]) {
  test(`rejects misplaced cloud source at ${path}`, (context) => {
    const root = repository(context);
    write(root, path, "// Synthetic implementation; no real data.\n");
    const reason = path.startsWith("landing/") && (path.includes("middleware") || path.includes("proxy")) ? "middleware or a proxy" : "private repository";
    hasViolation(root, path, reason);
  });
}

test("rejects ignored account routes outside Git's file listing", (context) => {
  const root = repository(context);
  write(root, ".gitignore", "landing/app/account/\n");
  write(root, "landing/app/account/page.tsx", "// Synthetic account route.\n");
  git(root, "check-ignore", "--quiet", "--", "landing/app/account/page.tsx");
  hasViolation(root, "landing/app/account/page.tsx", "account routes");
});

test("checks Git-index paths even if their working files have been deleted", (context) => {
  const root = repository(context);
  const path = "landing/app/sign-in/page.tsx";
  write(root, path, "// Synthetic account route.\n");
  git(root, "add", "--", path);
  rmSync(join(root, path));
  hasViolation(root, path, "account routes", "Git index");
});

test("ignored hosted API source cannot hide inside the public Rust API", (context) => {
  const root = repository(context);
  write(root, ".gitignore", "api/app/\n");
  write(root, "api/app/api/v1/ai/session-status/route.ts", "// Synthetic private handler.\n");
  hasViolation(root, "api/app/api/v1/ai/session-status/route.ts", "private repository");
});

test("hosted API source is checked in index and HEAD without blocking native Rust", (context) => {
  const root = repository(context);
  const path = "api/src/session-status/service.ts";
  write(root, "api/src/routes.rs", "// Synthetic local HTTP adapter.\n");
  write(root, path, "// Synthetic private hosted service.\n");
  git(root, "add", "--", "api");
  commit(root);
  rmSync(join(root, path));
  hasViolation(root, path, "private repository", "Git index");
  git(root, "add", "--", "api");
  hasViolation(root, path, "private repository", "HEAD");
});

test("a Clerk import cannot be moved from landing into the public local API", (context) => {
  const root = repository(context);
  write(root, "api/src/identity.ts", 'import "@clerk/backend";\n');
  hasViolation(root, "api/src/identity.ts", "Clerk cloud SDK");
});

for (const [path, manifest] of [
  ["landing/package.json", { dependencies: { "@clerk/nextjs": "1.0.0" } }],
  ["landing/package.json", { devDependencies: { "@clerk/backend": "1.0.0" } }],
  ["landing/package.json", { optionalDependencies: { clerk: "npm:@clerk/nextjs@1.0.0" } }],
  ["landing/package-lock.json", { packages: { "node_modules/@clerk/shared": { version: "1.0.0" } } }],
  ["landing/npm-shrinkwrap.json", { dependencies: { "@clerk/types": { version: "1.0.0" } } }],
]) {
  test(`rejects Clerk dependency in ${path}: ${JSON.stringify(manifest)}`, (context) => {
    const root = repository(context);
    write(root, path, JSON.stringify(manifest));
    hasViolation(root, path, "Clerk cloud SDK");
  });
}

for (const source of [
  'import { ClerkProvider } from "@clerk/nextjs";',
  'import "@clerk/nextjs";',
  'export { auth } from "@clerk/nextjs/server";',
  'const clerk = await import("@clerk/backend");',
  "const clerk = require('@clerk/backend');",
  'const clerk = require.resolve("@clerk/backend");',
]) {
  test(`rejects Clerk import: ${source}`, (context) => {
    const root = repository(context);
    write(root, "landing/lib/identity.ts", source);
    hasViolation(root, "landing/lib/identity.ts", "import a Clerk cloud SDK");
  });
}

for (const source of [
  'export default {};\n',
  'export default { output: "standalone" };\n',
  '// output: "export",\nexport default {};\n',
  '/*\noutput: "export",\n*/\nexport default {};\n',
  'export default { output: process.env.NEXT_OUTPUT };\n',
  'export default { output: "export", output: "standalone" };\n',
]) {
  test(`requires literal static export: ${JSON.stringify(source)}`, (context) => {
    const root = repository(context);
    write(root, "landing/next.config.ts", source);
    hasViolation(root, "landing/next.config.ts", "literal output");
  });
}

test("requires exactly one config and does not execute it", (context) => {
  const root = repository(context);
  write(root, "landing/next.config.ts", 'import "synthetic-module-that-does-not-exist";\nexport default { "output": \'export\' };\n');
  assert.deepEqual(checkPublicSourceBoundary(root), []);
  write(root, "landing/next.config.mjs", 'export default { output: "export" };\n');
  hasViolation(root, "landing", "exactly one");
  rmSync(join(root, "landing/next.config.ts"));
  rmSync(join(root, "landing/next.config.mjs"));
  hasViolation(root, "landing", "exactly one");
});

test("inspects index and HEAD contents, not just working-tree replacements", (context) => {
  const root = repository(context);
  write(root, "landing/package.json", '{"dependencies":{"@clerk/nextjs":"1.0.0"}}');
  write(root, "landing/lib/identity.ts", 'import "@clerk/nextjs";\n');
  write(root, "landing/next.config.ts", 'export default { output: "standalone" };\n');
  git(root, "add", "--", "landing");
  commit(root);
  write(root, "landing/package.json", '{"dependencies":{"next":"16.3.8"}}');
  write(root, "landing/lib/identity.ts", "// No account implementation.\n");
  write(root, "landing/next.config.ts", 'export default { output: "export" };\n');
  hasViolation(root, "landing/package.json", "Clerk cloud SDK", "Git index");
  hasViolation(root, "landing/lib/identity.ts", "Clerk cloud SDK", "Git index");
  hasViolation(root, "landing/next.config.ts", "literal output", "Git index");
  git(root, "add", "--", "landing");
  hasViolation(root, "landing/package.json", "Clerk cloud SDK", "HEAD");
  hasViolation(root, "landing/lib/identity.ts", "Clerk cloud SDK", "HEAD");
  hasViolation(root, "landing/next.config.ts", "literal output", "HEAD");
});

test("does not inspect older history after a clean current snapshot", (context) => {
  const root = repository(context);
  write(root, "cloud/server.ts", "Synthetic historic source.\n");
  git(root, "add", "--", "cloud");
  commit(root);
  git(root, "rm", "--quiet", "--", "cloud/server.ts");
  commit(root);
  assert.deepEqual(checkPublicSourceBoundary(root), []);
});

test("ignores generated landing output and environment files, without following links", (context) => {
  const root = repository(context);
  for (const directory of ["node_modules", ".next", "out", ".vercel"]) {
    write(root, `landing/${directory}/generated.js`, 'import "@clerk/nextjs";\n');
  }
  symlinkSync("missing-environment-file", join(root, "landing/.env.local"));
  assert.deepEqual(checkPublicSourceBoundary(root), []);
  write(root, "landing/out/published.js", 'import "@clerk/nextjs";\n');
  git(root, "add", "--", "landing/out/published.js");
  hasViolation(root, "landing/out/published.js", "Clerk cloud SDK", "Git index");
});

test("rejects landing source symlinks without reading their targets", (context) => {
  const root = repository(context);
  symlinkSync("missing-source", join(root, "landing/identity.ts"));
  hasViolation(root, "landing/identity.ts", "uninspected symlinks");
});

test("allows static route handlers, comments and URLs rather than scanning generic prose", (context) => {
  const root = repository(context);
  write(root, "landing/app/manifest.json/route.ts", 'export const dynamic = "force-static";\nexport function GET() { return Response.json({ name: "Rakumux" }); }\n');
  write(root, "landing/lib/public.ts", '// import "@clerk/nextjs" belongs in private source.\n/* export { auth } from "@clerk/backend"; */\nexport const repository = "https://github.com/humanunsupervised/rakumux-cloud";\n');
  assert.deepEqual(checkPublicSourceBoundary(root), []);
});

test("CLI reports deterministic diagnostics and a failing exit status", (context) => {
  const root = repository(context);
  const run = () => spawnSync(process.execPath, [guardPath, root], { encoding: "utf8", env: isolatedEnvironment(root) });
  const passed = run();
  assert.equal(passed.status, 0, passed.stderr);
  assert.match(passed.stdout, /not a secrets\/history audit/);
  write(root, "cloud/server.ts", "Synthetic private source.\n");
  const failed = run();
  assert.equal(failed.status, 1);
  assert.match(failed.stderr, /Public source boundary check failed/);
  assert.equal(failed.stderr, run().stderr);
  assert.equal(failed.stdout, "");
});

test("fails closed outside a repository without reading other trees", (context) => {
  const root = mkdtempSync(join(temporaryRoot, "rakumux-public-boundary-no-git-"));
  context.after(() => rmSync(root, { recursive: true, force: true }));
  assert.throws(() => checkPublicSourceBoundary(root), /Cannot inspect Git source/);
});
