#!/usr/bin/env node

// Rakumux's external HTTP API (127.0.0.1 only). Runs the rakumux-api
// binary from dist/ directly.
const path = require('path');
const { spawnSync } = require('child_process');

const installScript = path.join(__dirname, '..', 'install.js');
const result = spawnSync(process.execPath, [installScript, '--invoke=rakumux-api', ...process.argv.slice(2)], {
  stdio: 'inherit',
});
process.exit(result.status ?? 1);
