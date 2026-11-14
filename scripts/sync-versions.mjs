import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const root = fileURLToPath(new URL('../', import.meta.url));
export const cliPackages = ['luat', 'luat-win32-x64', 'luat-linux-x64', 'luat-linux-arm64', 'luat-darwin-x64', 'luat-darwin-arm64'];

export function manifestPath(product) {
  if (!['cli', 'lsp'].includes(product)) throw new Error(`Unknown product: ${product}`);
  return product === 'cli' ? 'Cargo.toml' : 'lsp/Cargo.toml';
}

export function parseVersion(cargo) {
  const value = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(value ?? '')) throw new Error('Expected a stable numeric major.minor.patch version');
  return value;
}

export async function versionFiles(directory, product = 'cli') {
  const manifest = manifestPath(product);
  const cargo = await readFile(path.join(directory, manifest), 'utf8');
  const version = parseVersion(cargo);
  const license = cargo.match(/^license\s*=\s*"([^"]+)"/m)?.[1];
  if (!license) throw new Error(`Missing license in ${manifest}`);
  const names = product === 'cli' ? cliPackages.map(name => `packages/${name}/package.json`)
    : ['lsp/editors/vscode/package.json', 'lsp/editors/vscode/package-lock.json'];
  const files = [];
  for (const name of names) {
    const value = JSON.parse(await readFile(path.join(directory, name), 'utf8'));
    if (name.endsWith('package-lock.json') && !value.packages?.['']) throw new Error('Missing extension lock root');
    files.push({ name, value });
  }
  return { manifest, cargo, version, license, files };
}

export async function syncVersions(directory = root, product = 'cli') {
  const { version, license, files } = await versionFiles(directory, product);
  for (const { name, value } of files) {
    value.version = version;
    if (name.endsWith('package-lock.json')) {
      value.packages[''].version = version;
      value.packages[''].license = license;
    } else {
      value.license = license;
      for (const dependency of Object.keys(value.optionalDependencies ?? {})) value.optionalDependencies[dependency] = version;
    }
    await writeFile(path.join(directory, name), `${JSON.stringify(value, null, name.includes('vscode') ? '\t' : 2)}\n`);
  }
}

export async function checkVersions(directory = root, product = 'cli') {
  const { version, license, files } = await versionFiles(directory, product);
  for (const { name, value } of files) {
    const metadata = name.endsWith('package-lock.json') ? value.packages[''] : value;
    if (value.version !== version || metadata.version !== version || metadata.license !== license
      || Object.values(value.optionalDependencies ?? {}).some(v => v !== version)) throw new Error(`Version/license mismatch in ${name}; run sync:versions:${product}`);
  }
  return version;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const args = process.argv.slice(2);
  const check = args.includes('--check');
  const product = args.filter(arg => arg !== '--check')[0] ?? 'cli';
  await (check ? checkVersions : syncVersions)(root, product);
}
