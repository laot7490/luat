import { writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { root, syncVersions, versionFiles } from './sync-versions.mjs';

export async function prepareVersion(directory, bump, product = 'cli') {
  if (!['major', 'minor', 'patch'].includes(bump)) throw new Error('Unknown bump; use major, minor or patch');
  const { manifest, cargo, version } = await versionFiles(directory, product);
  let [major, minor, patch] = version.split('.').map(Number);
  if (bump === 'major') [major, minor, patch] = [major + 1, 0, 0];
  if (bump === 'minor') [minor, patch] = [minor + 1, 0];
  if (bump === 'patch') patch++;
  const next = `${major}.${minor}.${patch}`;
  await writeFile(path.join(directory, manifest), cargo.replace(/^(version\s*=\s*")[^"]+(".*)$/m, (_, prefix, suffix) => `${prefix}${next}${suffix}`));
  await syncVersions(directory, product);
  const result = spawnSync('cargo', ['check', '--workspace', '--offline'], { cwd: directory, stdio: 'inherit' });
  if (result.error || result.status !== 0) throw new Error(result.error?.message ?? `cargo check failed (${result.status}); version changes remain for inspection`);
  return next;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) console.log(await prepareVersion(root, process.argv[2], process.argv[3] ?? 'cli'));
