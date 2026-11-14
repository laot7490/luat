import assert from 'node:assert/strict';
import { inspectVsix } from './package-lsp.mjs';

const platform = 'win32-x64';
const required = [
  'extension/out/extension.js',
  'extension/server/win32-x64/luat-lsp.exe',
  ...['lua-shared.d.luat', 'lua-server.d.luat', 'fivem-shared.d.luat', 'fivem-client.d.luat', 'fivem-server.d.luat', 'runtime-shared.d.luat', 'runtime-client.d.luat', 'runtime-server.d.luat', 'LICENSE', 'SOURCE.md', 'LUA-SOURCE.md']
    .map(name => `extension/server/${platform}/definitions/${name}`),
];
const manifest = { version: '0.1.0' };

assert.doesNotThrow(() => inspectVsix(required, manifest, platform, '0.1.0'));
assert.throws(() => inspectVsix(required.filter(name => name !== 'extension/out/extension.js'), manifest, platform, '0.1.0'), /Missing VSIX file/);
assert.throws(() => inspectVsix([...required, 'extension/node_modules/foo/index.js'], manifest, platform, '0.1.0'), /dependencies or tests/);
assert.throws(() => inspectVsix(required, manifest, 'linux-x64', '0.1.0'), /another platform/);
