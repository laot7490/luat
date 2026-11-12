import { readFile, writeFile } from 'node:fs/promises';

const cargo = await readFile(new URL('../Cargo.toml', import.meta.url), 'utf8');
const version = cargo.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (!version) throw new Error('Could not read package version from Cargo.toml');
const license = cargo.match(/^license\s*=\s*"([^"]+)"/m)?.[1];

for (const name of ['@luatlang/cli', 'luat-win32-x64', 'luat-linux-x64', 'luat-linux-arm64', 'luat-darwin-x64', 'luat-darwin-arm64']) {
  const path = new URL(`../packages/${name === '@luatlang/cli' ? 'luat' : name}/package.json`, import.meta.url);
  const pkg = JSON.parse(await readFile(path, 'utf8'));
  pkg.version = version;
  if (license) pkg.license = license;
  if (name === '@luatlang/cli') {
    for (const dependency of Object.keys(pkg.optionalDependencies)) pkg.optionalDependencies[dependency] = version;
  }
  await writeFile(path, `${JSON.stringify(pkg, null, 2)}\n`);
}
