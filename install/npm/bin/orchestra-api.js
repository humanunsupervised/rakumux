#!/usr/bin/env node

// Orchestra's external HTTP API (127.0.0.1 only). Runs the orchestra-api
// binary from dist/ directly.
const path = require('path');
const { spawnSync } = require('child_process');

const installScript = path.join(__dirname, '..', 'install.js');
const result = spawnSync(process.execPath, [installScript, '--invoke=orchestra-api', ...process.argv.slice(2)], {
  stdio: 'inherit',
});
process.exit(result.status ?? 1);
