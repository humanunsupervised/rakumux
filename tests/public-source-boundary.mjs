#!/usr/bin/env node
// Developer-only source boundary guard. Not secret detection or an audit of Git history.
// Inspect known paths/imports and a literal static-export declaration; never execute config.
import { spawnSync } from "node:child_process";
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const privateRoots = new Set(["cloud", "rakumux-cloud"]);
const generatedDirectories = new Set([".git", ".next", ".vercel", "node_modules", "out", "coverage", "target"]);
const accountRoutes = new Set(["account", "auth", "sign-in", "sign-up", "connect", "welcome", "billing"]);
const scriptExtension = /\.(?:[cm]?[jt]s|[jt]sx)$/;
const nextConfigPath = /^landing\/next\.config\.(?:[cm]?[jt]s)$/;
const packageManifestPath = /(?:^|\/)(?:package(?:-lock)?\.json|npm-shrinkwrap\.json)$/;

function isEnvironmentFile(name) {
  return name === ".env" || name.startsWith(".env.");
}

function existsWithoutFollowingLinks(path) {
  try {
    return lstatSync(path);
  } catch (error) {
    if (error.code === "ENOENT") return undefined;
    throw error;
  }
}

function workingTreePaths(root, report) {
  const paths = [];
  for (const name of privateRoots) {
    if (existsWithoutFollowingLinks(join(root, name))) paths.push(name);
  }

  function visit(relativePath) {
    const stat = existsWithoutFollowingLinks(join(root, relativePath));
    if (!stat) return;
    paths.push(relativePath);
    if (stat.isSymbolicLink()) {
      report(relativePath, "public application source must not use uninspected symlinks");
    } else if (stat.isDirectory()) {
      for (const entry of readdirSync(join(root, relativePath)).sort()) {
        if (generatedDirectories.has(entry) || isEnvironmentFile(entry)) continue;
        visit(`${relativePath}/${entry}`);
      }
    } else if (!stat.isFile()) {
      report(relativePath, "public application source must be a regular file or directory");
    }
  }

  visit("landing");
  visit("api");
  return paths;
}

function prohibitedPath(path) {
  if (privateRoots.has(path.split("/")[0])) {
    return "private cloud source belongs in the separate private repository";
  }
  if (/^api\/(?:src\/)?(?:next\.config|middleware|proxy)\.[cm]?[jt]sx?$/.test(path)
      || (/^api\/(?:src\/)?(?:app|pages|session-status)\//.test(path) && scriptExtension.test(path))
      || /^api\/src\/production\.[cm]?[jt]s$/.test(path)) {
    return "hosted API implementation belongs in the private repository, not the local Rust API";
  }
  if (/^landing\/(?:src\/)?(?:middleware|proxy)\.[cm]?[jt]sx?$/.test(path)
      || /^landing\/(?:src\/)?pages\/(?:.*\/)?_middleware\.[cm]?[jt]sx?$/.test(path)) {
    return "public static landing must not contain middleware or a proxy";
  }
  if (/^landing\/(?:src\/)?(?:lib|components)\/account(?:\/|$)/.test(path)) {
    return "website account implementation belongs in the private repository";
  }

  const route = path.match(/^landing\/(?:src\/)?(app|pages)\/(.+)$/);
  if (!route) return undefined;
  const segments = route[2].split("/");
  const routeSegments = segments.filter((segment) => !/^\(.+\)$/.test(segment) && !segment.startsWith("@"));
  const handler = route[1] === "app"
    ? /^route\.[cm]?[jt]sx?$/.test(segments.at(-1))
    : routeSegments[0] === "api" && scriptExtension.test(path);
  if (handler && routeSegments.some((segment) => ["ai", "billing", "stripe"].includes(segment))) {
    return "AI and billing route handlers belong in the private repository";
  }
  const firstRoute = routeSegments[0]?.replace(scriptExtension, "");
  if (segments.includes("(account)") || accountRoutes.has(firstRoute)) {
    return "website account routes belong in the private repository";
  }
  return undefined;
}

// Preserve quoted strings (including URLs) while discarding ordinary JS/TS comments.
// This is deliberately a syntax-pattern guard, not a JavaScript semantic analyzer.
function withoutComments(source) {
  return source.replace(
    /("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`)|\/\*[\s\S]*?\*\/|\/\/[^\r\n]*/g,
    (match, quoted) => quoted ?? " ",
  );
}

function inspectContents(path, source, report) {
  if (packageManifestPath.test(path)) {
    let manifest;
    try {
      manifest = JSON.stringify(JSON.parse(source));
    } catch {
      report(path, "cannot inspect invalid package manifest JSON");
      return;
    }
    if (/"(?:[^"\n]*\/)?@clerk\/[^"\n]*"\s*:|npm:@clerk\//.test(manifest)) {
      report(path, "public application source must not depend on a Clerk cloud SDK");
    }
    return;
  }

  const code = withoutComments(source);
  if (/\b(?:from\s*|(?:import|require)(?:\.resolve)?\s*(?:\(\s*)?)["'`]@clerk\//.test(code)) {
    report(path, "public application source must not import a Clerk cloud SDK");
  }
  if (nextConfigPath.test(path)) {
    const outputs = [...code.matchAll(/(?:\boutput|["']output["'])\s*:\s*([^,\r\n}]+)/g)];
    if (outputs.length !== 1 || !/^(?:"export"|'export')$/.test(outputs[0][1].trim())) {
      report(path, 'public landing requires one literal output: "export" declaration');
    }
  }
}

function inspectSnapshot(paths, readSource, report, requireLanding = false) {
  const landingPaths = paths.filter((path) => path.startsWith("landing/"));
  if (requireLanding || landingPaths.length > 0) {
    if (landingPaths.filter((path) => nextConfigPath.test(path)).length !== 1) {
      report("landing", "public landing requires exactly one Next.js static-export config");
    }
  }

  for (const path of [...new Set(paths)].sort()) {
    const reason = prohibitedPath(path);
    if (reason) {
      report(path, reason);
      continue; // A path violation needs no inspection of potentially private contents.
    }
    if ((!path.startsWith("landing/") && !path.startsWith("api/")) || path.split("/").some(isEnvironmentFile)) continue;
    if (!scriptExtension.test(path) && !packageManifestPath.test(path)) continue;
    const source = readSource(path);
    if (source !== undefined) inspectContents(path, source, report);
  }
}

function git(root, args, allowUnbornHead = false) {
  const result = spawnSync("git", ["-c", "core.fsmonitor=false", "-C", root, ...args], {
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
    // Do not consult user config, inherited Git overrides, credential helpers or hooks.
    env: { PATH: process.env.PATH, GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null", LC_ALL: "C" },
  });
  if (allowUnbornHead && result.status === 1) return undefined;
  if (result.error || result.status !== 0) {
    throw new Error(`Cannot inspect Git source (${args[0]}); run from a readable repository root`);
  }
  return result.stdout;
}

/** Check the working tree, Git index and current HEAD, never older commits or other refs. */
export function checkPublicSourceBoundary(directory) {
  const root = resolve(directory);
  if (resolve(git(root, ["rev-parse", "--show-toplevel"]).trim()) !== root) {
    throw new Error("The source boundary check requires the repository root");
  }
  const violations = new Set();
  const reporter = (snapshot) => (path, reason) => violations.add(`${snapshot}: ${JSON.stringify(path)}: ${reason}`);
  const workingReport = reporter("working tree");
  const paths = workingTreePaths(root, workingReport);
  inspectSnapshot(paths, (path) => {
    const stat = lstatSync(join(root, path));
    return stat.isFile() ? readFileSync(join(root, path), "utf8") : undefined;
  }, workingReport, true);

  const indexPaths = git(root, ["ls-files", "--cached", "-z"]).split("\0").filter(Boolean);
  inspectSnapshot(indexPaths, (path) => git(root, ["show", `:${path}`]), reporter("Git index"));
  if (git(root, ["rev-parse", "--verify", "--quiet", "HEAD"], true) !== undefined) {
    const headPaths = git(root, ["ls-tree", "-r", "--name-only", "-z", "HEAD"]).split("\0").filter(Boolean);
    inspectSnapshot(headPaths, (path) => git(root, ["show", `HEAD:${path}`]), reporter("HEAD"));
  }
  return [...violations].sort();
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    if (process.argv.length > 3) throw new Error("Usage: node tests/public-source-boundary.mjs [repository-root]");
    const root = process.argv[2] ?? fileURLToPath(new URL("../", import.meta.url));
    const violations = checkPublicSourceBoundary(root);
    if (violations.length > 0) {
      console.error(`Public source boundary check failed:\n${violations.map((entry) => `- ${entry}`).join("\n")}`);
      process.exitCode = 1;
    } else {
      console.log("Public source boundary passed (working tree, Git index and HEAD; not a secrets/history audit).");
    }
  } catch (error) {
    console.error(`Public source boundary check failed: ${error.message}`);
    process.exitCode = 1;
  }
}
