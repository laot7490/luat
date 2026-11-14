import { chmod, copyFile, cp, mkdir, mkdtemp, readFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export const platforms = ['win32-x64', 'linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64'];
const definitions = ['lua-shared.d.luat', 'lua-server.d.luat', 'fivem-shared.d.luat', 'fivem-client.d.luat', 'fivem-server.d.luat', 'runtime-shared.d.luat', 'runtime-client.d.luat', 'runtime-server.d.luat', 'LICENSE', 'SOURCE.md', 'LUA-SOURCE.md'];

export function inspectVsix(entries, manifest, platform, version) {
  if (!platforms.includes(platform)) throw new Error(`Unsupported platform: ${platform}`);
  if (manifest.version !== version) throw new Error('VSIX version differs from language server version');
  if (entries.some(name => name.startsWith('extension/server/') && !name.startsWith(`extension/server/${platform}/`))) throw new Error('VSIX includes another platform');
  const required = ['extension/out/extension.js',
    ...['vscode-languageclient', 'vscode-jsonrpc', 'vscode-languageserver-protocol', 'vscode-languageserver-types', 'semver'].map(name => `extension/node_modules/${name}/package.json`),
    `extension/server/${platform}/luat-lsp${platform.startsWith('win32') ? '.exe' : ''}`,
    ...definitions.map(name => `extension/server/${platform}/definitions/${name}`)];
  for (const name of required) if (!entries.includes(name)) throw new Error(`Missing VSIX file: ${name}`);
}

export async function packageLsp(root, platform, binary, output) {
  if (!platforms.includes(platform)) throw new Error(`Unsupported platform: ${platform}`);
  const extension = path.join(root, 'lsp/editors/vscode');
  const require = createRequire(path.join(extension, 'package.json'));
  const vsce = require('@vscode/vsce');
  const zip = createRequire(require.resolve('@vscode/vsce/package.json'))('yauzl');
  const parent = path.join(root, 'target/vsix-staging');
  await mkdir(parent, { recursive: true });
  const stage = await mkdtemp(path.join(parent, `${platform}-`));
  for (const name of ['package.json', 'README.md', 'LICENSE', 'language-configuration.json', 'images', 'syntaxes']) await cp(path.join(extension, name), path.join(stage, name), { recursive: true });
  await mkdir(path.join(stage, 'out'), { recursive: true });
  for (const name of ['extension.js', 'extension.js.map']) await copyFile(path.join(extension, 'out', name), path.join(stage, 'out', name));
  const result = spawnSync('npm ls --omit=dev --parseable --all', { cwd: extension, encoding: 'utf8', shell: true });
  if (result.error || result.status !== 0) throw new Error(result.error?.message ?? result.stderr);
  const dependencies = new Set();
  for (const dependency of result.stdout.trim().split(/\r?\n/).slice(1)) {
    const relative = path.relative(extension, dependency);
    if (!relative.startsWith(`node_modules${path.sep}`)) throw new Error('Dependency is outside the extension');
    const parts = relative.split(path.sep);
    dependencies.add(parts.slice(0, parts[1].startsWith('@') ? 3 : 2).join(path.sep));
  }
  for (const name of dependencies) await cp(path.join(extension, name), path.join(stage, name), { recursive: true });
  const server = path.join(stage, 'server', platform);
  await mkdir(path.join(server, 'definitions'), { recursive: true });
  const executable = `luat-lsp${platform.startsWith('win32') ? '.exe' : ''}`;
  await copyFile(binary, path.join(server, executable));
  if (!platform.startsWith('win32')) await chmod(path.join(server, executable), 0o755);
  for (const name of definitions) await copyFile(path.join(root, 'lsp/definitions', name), path.join(server, 'definitions', name));
  await mkdir(output, { recursive: true });
  const vsix = path.join(output, `luat-vscode-${platform}.vsix`);
  await vsce.createVSIX({ cwd: stage, target: platform, packagePath: vsix, useYarn: false });
  const entries = [];
  let manifest;
  let xml;
  let executableMode;
  await new Promise((resolve, reject) => zip.open(vsix, { lazyEntries: true }, (error, archive) => {
    if (error) return reject(error);
    archive.on('error', reject);
    archive.on('end', resolve);
    archive.on('entry', entry => {
      entries.push(entry.fileName);
      if (entry.fileName === `extension/server/${platform}/${executable}`) executableMode = entry.externalFileAttributes >>> 16;
      if (!['extension/package.json', 'extension.vsixmanifest'].includes(entry.fileName)) return archive.readEntry();
      archive.openReadStream(entry, (streamError, stream) => {
        if (streamError) return reject(streamError);
        const chunks = [];
        stream.on('error', reject);
        stream.on('data', chunk => chunks.push(chunk));
        stream.on('end', () => {
          const content = Buffer.concat(chunks).toString('utf8');
          if (entry.fileName === 'extension/package.json') manifest = JSON.parse(content);
          else xml = content;
          archive.readEntry();
        });
      });
    });
    archive.readEntry();
  }));
  const version = JSON.parse(await readFile(path.join(extension, 'package.json'), 'utf8')).version;
  inspectVsix(entries, manifest, platform, version);
  if (!xml?.includes(`TargetPlatform="${platform}"`)) throw new Error('VSIX target platform differs');
  if (process.platform !== 'win32' && !platform.startsWith('win32') && !(executableMode & 0o111)) throw new Error('VSIX server is not executable');
  const archive = path.join(output, `luat-lsp-${platform}.tar.gz`);
  const packed = spawnSync('tar', ['-czf', archive, '-C', server, '.'], { stdio: 'inherit' });
  if (packed.error || packed.status !== 0) throw new Error('Could not archive language server');
  console.log(`Inspected ${vsix}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [platform, binary, output] = process.argv.slice(2);
  await packageLsp(process.cwd(), platform, path.resolve(binary), path.resolve(output));
}
