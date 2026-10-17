#!/usr/bin/env node

const path = require('path');
const { spawnSync } = require('child_process');

const installScript = path.join(__dirname, '..', 'install.js');
const result = spawnSync(process.execPath, [installScript, '--invoke=rakumux-local', ...process.argv.slice(2)], {
  stdio: 'inherit',
});
process.exit(result.status ?? 1);
