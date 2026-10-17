#!/usr/bin/env node

// Orchestra's MCP server for coding agents (stdio). Runs the orchestra-mcp
// binary from dist/ directly; stdout carries MCP messages only. For faster
// startup, agents can run $(npm root -g)/@humanu/orchestra/dist/orchestra-mcp.
const path = require('path');
const { spawnSync } = require('child_process');

const installScript = path.join(__dirname, '..', 'install.js');
const result = spawnSync(process.execPath, [installScript, '--invoke=orchestra-mcp', ...process.argv.slice(2)], {
  stdio: 'inherit',
});
process.exit(result.status ?? 1);
