#!/usr/bin/env node
'use strict';

const { spawn } = require('node:child_process');

const packages = {
  'win32-x64': 'luat-win32-x64',
  'linux-x64': 'luat-linux-x64',
  'linux-arm64': 'luat-linux-arm64',
  'darwin-x64': 'luat-darwin-x64',
  'darwin-arm64': 'luat-darwin-arm64',
};

function launch() {
  const key = `${process.platform}-${process.arch}`;
  const binaryPackage = packages[key];
  if (!binaryPackage) {
    console.error(`\nerror: LuaT has no binary for ${key}.`);
    process.exitCode = 2;
    return;
  }

  let binary;
  try {
    binary = require.resolve(binaryPackage);
  } catch {
    console.error(`\nerror: LuaT binary package '${binaryPackage}' is missing. Reinstall luat for ${key}.`);
    process.exitCode = 2;
    return;
  }

  const child = spawn(binary, process.argv.slice(2), {
    cwd: process.cwd(),
    env: process.env,
    stdio: 'inherit',
    shell: false,
  });

  let spawnFailed = false;
  child.once('error', (error) => {
    spawnFailed = true;
    process.exitCode = 2;
    console.error(`\nerror: Could not start the LuaT compiler: ${error.message}`);
  });
  for (const signal of ['SIGINT', 'SIGTERM']) {
    process.once(signal, () => child.kill(signal));
  }
  child.once('close', (code, signal) => {
    if (!spawnFailed) process.exitCode = code ?? (signal === 'SIGINT' ? 130 : signal === 'SIGTERM' ? 143 : 1);
  });
}

launch();
