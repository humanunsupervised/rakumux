#!/usr/bin/env node

// Postinstall setup and command runner for the rakumux npm package.
//
// Package layout (staged by install/scripts/stage-npm-package.sh):
//   resources/scripts/            rakumux.sh, rakumux-local.sh, shell/
//   resources/prebuilt/<platform>/ rakumux (tui binary), rakumux-ptyd,
//                                  rakumux-api, rakumux-mcp
// Postinstall copies the scripts and this platform's binaries into dist/, and
// bin/*.js run `node install.js --invoke=<command> [args...]`, which execs the
// matching script from dist/ (rakumux, rakumux-local) or the binary itself
// (rakumux-mcp, rakumux-api: no launcher, and nothing but the binary may
// write to stdout, which carries MCP messages).
//
// Inside a source checkout (install/npm in the rakumux repo) the scripts
// come from the repo and the binaries are built with cargo.

const { execSync, spawnSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const BINARIES = [
  // [name in dist/ and resources/prebuilt/<platform>/, cargo binary name]
  ['rakumux', 'tui'],
  ['rakumux-ptyd', 'rakumux-ptyd'],
  ['rakumux-api', 'rakumux-api'],
  ['rakumux-mcp', 'rakumux-mcp'],
];
const SUPPORTED_PLATFORMS = {
  'darwin-x64': 'macos-intel',
  'darwin-arm64': 'macos-arm64',
  'linux-x64': 'linux-x64',
};
const COMMANDS = {
  rakumux: 'rakumux.sh',
  'rakumux-local': 'rakumux-local.sh',
};
// Commands that run a binary from dist/ directly.
const BINARY_COMMANDS = {
  'rakumux-mcp': 'rakumux-mcp',
  'rakumux-api': 'rakumux-api',
};

// Progress messages. Binary commands send them to stderr: an MCP server's
// stdout carries protocol messages only.
let log = console.log;

const packageRoot = __dirname;
const resourcesDir = path.join(packageRoot, 'resources');
const scriptsDir = path.join(resourcesDir, 'scripts');
const prebuiltDir = path.join(resourcesDir, 'prebuilt');
const distDir = path.join(packageRoot, 'dist');
const projectRoot = path.resolve(packageRoot, '..', '..');
const hasLocalSource =
  fs.existsSync(path.join(projectRoot, 'tui', 'Cargo.toml')) &&
  fs.existsSync(path.join(projectRoot, 'rakumux.sh'));

function ensureDir(dir) {
  fs.mkdirSync(dir, { recursive: true });
}

function cleanDir(dir) {
  fs.rmSync(dir, { recursive: true, force: true });
  ensureDir(dir);
}

function fail(lines) {
  for (const line of lines) {
    console.error(line);
  }
  process.exit(1);
}

function copyRecursive(src, dest) {
  const stats = fs.statSync(src);
  if (stats.isDirectory()) {
    ensureDir(dest);
    for (const entry of fs.readdirSync(src)) {
      copyRecursive(path.join(src, entry), path.join(dest, entry));
    }
  } else {
    ensureDir(path.dirname(dest));
    fs.copyFileSync(src, dest);
    fs.chmodSync(dest, 0o755);
  }
}

function copyExecutable(src, dest) {
  fs.copyFileSync(src, dest);
  fs.chmodSync(dest, 0o755);
}

// [source, destination in dist/] pairs for the launcher scripts.
function scriptSources() {
  if (fs.existsSync(scriptsDir)) {
    return [
      [path.join(scriptsDir, 'rakumux.sh'), 'rakumux.sh'],
      [path.join(scriptsDir, 'rakumux-local.sh'), 'rakumux-local.sh'],
      [path.join(scriptsDir, 'shell'), 'shell'],
    ];
  }
  if (hasLocalSource) {
    return [
      [path.join(projectRoot, 'rakumux.sh'), 'rakumux.sh'],
      [path.join(projectRoot, 'shell', 'rakumux-local.sh'), 'rakumux-local.sh'],
      [path.join(projectRoot, 'shell'), 'shell'],
    ];
  }
  fail(['Rakumux launcher scripts are missing from the package (resources/scripts).']);
  return [];
}

function copyScripts() {
  for (const [source, destination] of scriptSources()) {
    if (!fs.existsSync(source)) {
      fail([`Required file missing: ${source}`]);
    }
    copyRecursive(source, path.join(distDir, destination));
  }
}

function platformKey() {
  return `${process.platform}-${process.arch}`;
}

function installPrebuiltBinaries() {
  const platform = SUPPORTED_PLATFORMS[platformKey()];
  if (!platform) {
    return false;
  }
  const sourceDir = path.join(prebuiltDir, platform);
  if (!BINARIES.every(([name]) => fs.existsSync(path.join(sourceDir, name)))) {
    return false;
  }
  for (const [name] of BINARIES) {
    copyExecutable(path.join(sourceDir, name), path.join(distDir, name));
  }
  return true;
}

function buildFromSource() {
  try {
    execSync('cargo --version', { stdio: 'ignore' });
  } catch (error) {
    fail(['❌ Rust toolchain not found; install it from https://rustup.rs/ to build from source.']);
  }

  const tuiDir = path.join(projectRoot, 'tui');
  log('🔨 Building rakumux, rakumux-ptyd, rakumux-api and rakumux-mcp from source...');
  try {
    execSync('cargo build --release -p tui -p rakumux-ptyd -p rakumux-api -p rakumux-mcp', {
      stdio: ['ignore', 2, 2],
      cwd: tuiDir,
    });
  } catch (error) {
    fail(['❌ cargo build failed (see above).']);
  }

  for (const [name, cargoName] of BINARIES) {
    const built = path.join(tuiDir, 'target', 'release', cargoName);
    if (!fs.existsSync(built)) {
      fail([`❌ Build finished but ${cargoName} was not produced at ${built}`]);
    }
    copyExecutable(built, path.join(distDir, name));
  }
  log('✅ Built from source');
}

function installBinaries() {
  if (installPrebuiltBinaries()) {
    return;
  }
  if (hasLocalSource) {
    buildFromSource();
    return;
  }
  fail([
    `❌ Rakumux has no prebuilt binaries for ${platformKey()}.`,
    '   Supported: macOS (Apple Silicon, Intel) and Linux x64.',
    '   Releases: https://github.com/humanunsupervised/rakumux/releases',
  ]);
}

function assetsReady() {
  return (
    BINARIES.every(([name]) => fs.existsSync(path.join(distDir, name))) &&
    fs.existsSync(path.join(distDir, 'rakumux.sh')) &&
    fs.existsSync(path.join(distDir, 'shell', 'gwr_load.sh'))
  );
}

function performSetup({ quiet = false } = {}) {
  if (!quiet) {
    log('Installing Rakumux...');
  }
  cleanDir(distDir);
  copyScripts();
  installBinaries();
  if (!quiet) {
    log('✅ Rakumux installed. Run `rakumux` in any project folder.');
  }
}

function runBinary(command, args) {
  log = console.error;
  if (!assetsReady()) {
    performSetup();
  }
  const binaryPath = path.join(distDir, BINARY_COMMANDS[command]);
  if (!fs.existsSync(binaryPath)) {
    fail([`Missing binary: ${binaryPath}`]);
  }
  // Pin this package's binaries, as for the rakumux command.
  const env = {
    ...process.env,
    RAKUMUX_API_BIN: path.join(distDir, 'rakumux-api'),
    RAKUMUX_PTYD_BIN: path.join(distDir, 'rakumux-ptyd'),
  };
  const result = spawnSync(binaryPath, args, { stdio: 'inherit', env });
  if (result.error) {
    fail([`Failed to launch ${command}: ${result.error.message}`]);
  }
  process.exit(result.status ?? 1);
}

function runCommand(command, args) {
  if (Object.prototype.hasOwnProperty.call(BINARY_COMMANDS, command)) {
    runBinary(command, args);
    return;
  }
  if (!assetsReady()) {
    performSetup();
  }

  const scriptPath = path.join(distDir, COMMANDS[command]);
  if (!fs.existsSync(scriptPath)) {
    fail([`Missing script: ${scriptPath}`]);
  }

  const env = { ...process.env };
  if (command === 'rakumux') {
    // Pin this package's binaries; values inherited from another install
    // (e.g. a session started by a different Rakumux) must not win.
    env.GW_TUI_BIN = path.join(distDir, 'rakumux');
    env.GW_PTYD_BIN = path.join(distDir, 'rakumux-ptyd');
  }

  const result = spawnSync('bash', [scriptPath, ...args], { stdio: 'inherit', env });
  if (result.error) {
    fail([`Failed to launch ${command}: ${result.error.message}`]);
  }
  process.exit(result.status ?? 1);
}

function main() {
  if (process.env.npm_lifecycle_event === 'postinstall') {
    performSetup();
    return;
  }

  // bin/*.js always pass --invoke=<command> first; later arguments belong to the user.
  const [first, ...rest] = process.argv.slice(2);
  if (first && first.startsWith('--invoke=')) {
    const command = first.slice('--invoke='.length);
    const known =
      Object.prototype.hasOwnProperty.call(COMMANDS, command) ||
      Object.prototype.hasOwnProperty.call(BINARY_COMMANDS, command);
    if (!known) {
      fail([`Unknown command: ${command}`]);
    }
    runCommand(command, rest);
    return;
  }

  // `node install.js` run by hand: set up dist/.
  performSetup();
}

main();
