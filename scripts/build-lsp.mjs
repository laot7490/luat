import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { packageLsp } from './package-lsp.mjs';

const root = new URL('../', import.meta.url);
const extension = new URL('lsp/editors/vscode/', root);
const platform = `${process.platform}-${process.arch}`;
const executable = process.platform === 'win32' ? 'luat-lsp.exe' : 'luat-lsp';

function run(command, args, cwd, shell = false) {
  const result = spawnSync(command, args, { cwd, shell, stdio: 'inherit' });
  if (result.error) console.error(result.error.message);
  if (result.error || result.status !== 0) process.exit(result.status || 1);
}

for (const dependency of ['typescript/bin/tsc', 'esbuild/bin/esbuild', '@vscode/vsce/vsce', 'vscode-languageclient/package.json']) {
  if (!existsSync(new URL(`node_modules/${dependency}`, extension))) {
    console.error('Missing extension dependencies. Run: npm --prefix lsp/editors/vscode ci');
    process.exit(1);
  }
}

run('cargo', ['build', '--release', '-p', 'luat-lsp', '--locked'], root);

run(process.platform === 'win32' ? 'npm.cmd' : 'npm', ['run', 'compile'], fileURLToPath(extension), process.platform === 'win32');
await packageLsp(fileURLToPath(root), platform, fileURLToPath(new URL(`target/release/${executable}`, root)), fileURLToPath(new URL('target/release-assets/', root)));
