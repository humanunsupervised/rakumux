#!/usr/bin/env node

// Every argument goes to orchestra.sh (same as Homebrew): no command opens the
// TUI, anything else (new, ls, attach, session, daemon, ...) is a CLI subcommand.
const path = require('path');
const { spawnSync } = require('child_process');

const installScript = path.join(__dirname, '..', 'install.js');
const result = spawnSync(process.execPath, [installScript, '--invoke=orchestra', ...process.argv.slice(2)], {
  stdio: 'inherit',
});
process.exit(result.status ?? 1);
