#!/usr/bin/env node
'use strict';

const { spawn } = require('node:child_process');
const { version } = require('../package.json');

const packages = {
  'win32-x64': 'luat-win32-x64',
  'linux-x64': 'luat-linux-x64',
  'linux-arm64': 'luat-linux-arm64',
  'darwin-x64': 'luat-darwin-x64',
  'darwin-arm64': 'luat-darwin-arm64',
};

function reportError(message) {
  const args = process.argv.slice(2);
  const plain = args.some((arg, index) => arg === '--pretty' && args[index + 1] === 'false');
  const color = process.stderr.isTTY && !Object.hasOwn(process.env, 'NO_COLOR') && !plain;
  const tag = `luat v${version}`;
  console.error(color
    ? `\x1b[1;38;2;157;130;241m${tag}\x1b[0m \x1b[31m${message}\x1b[0m`
    : `${tag} ${message}`);
}

function launch() {
  const key = `${process.platform}-${process.arch}`;
  const binaryPackage = packages[key];
  if (!binaryPackage) {
    reportError(`LuaT has no binary for ${key}.`);
    process.exitCode = 2;
    return;
  }

  let binary;
  try {
    binary = require.resolve(binaryPackage);
  } catch {
    reportError(`LuaT binary package '${binaryPackage}' is missing. Reinstall luat for ${key}.`);
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
    reportError(`Could not start the LuaT compiler: ${error.message}`);
  });
  for (const signal of ['SIGINT', 'SIGTERM']) {
    process.once(signal, () => child.kill(signal));
  }
  child.once('close', (code, signal) => {
    if (!spawnFailed) process.exitCode = code ?? (signal === 'SIGINT' ? 130 : signal === 'SIGTERM' ? 143 : 1);
  });
}

launch();
