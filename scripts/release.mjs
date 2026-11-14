import { appendFile, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { checkVersions, manifestPath, parseVersion } from './sync-versions.mjs';

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', ...options });
  if (result.error || result.status !== 0) {
    const error = new Error(result.error?.message ?? `${command} failed: ${result.stderr || result.stdout}`);
    error.status = result.status;
    throw error;
  }
  return result.stdout.trim();
}
const git = args => run('git', args);
const version = value => parseVersion(`version = "${value}"`);
export function compareVersions(a, b) {
  const left = version(a).split('.').map(BigInt);
  const right = version(b).split('.').map(BigInt);
  for (let i = 0; i < 3; i++) if (left[i] !== right[i]) return left[i] > right[i] ? 1 : -1;
  return 0;
}

export function detectReleases(before, after) {
  return ['cli', 'lsp'].filter(product => {
    version(after[product]);
    if (before[product] == null) return true;
    if (compareVersions(after[product], before[product]) < 0) throw new Error(`${product} version must not decrease`);
    return after[product] !== before[product];
  });
}

export function tagVersion(product, tag) {
  manifestPath(product);
  const prefix = product === 'cli' ? 'v' : 'lsp-v';
  if (!tag?.startsWith(prefix)) throw new Error(`Expected ${prefix}MAJOR.MINOR.PATCH tag`);
  return version(tag.slice(prefix.length));
}

function validateSha(sha) {
  if (!/^[a-f0-9]{40}$/.test(sha ?? '') || /^0+$/.test(sha)) throw new Error('Expected an existing nonzero commit SHA');
  return sha;
}

export function resolveSource(product, tag, sourceSha, gitCommand = git) {
  tagVersion(product, tag);
  if (sourceSha) validateSha(sourceSha);
  let tagged;
  try { tagged = gitCommand(['rev-parse', '--verify', `refs/tags/${tag}^{commit}`]); }
  catch (error) { if (!sourceSha || error.status !== 128) throw error; }
  if (tagged && sourceSha && tagged !== sourceSha) throw new Error(`${tag} points to a different commit; recover from the existing tag`);
  const sha = validateSha(tagged ?? sourceSha);
  gitCommand(['merge-base', '--is-ancestor', sha, 'origin/master']);
  return sha;
}

export async function publishPackages(packages, { registry, latest, publish }) {
  const missing = [];
  for (const pkg of packages) {
    const existing = await registry(pkg);
    if (existing) {
      if (existing.dist?.integrity !== pkg.integrity) throw new Error(`Registry integrity differs for ${pkg.name}@${pkg.version}; recover the original prepared artifact`);
    } else {
      const current = await latest(pkg);
      if (current && compareVersions(pkg.version, current) < 0) throw new Error(`Refusing to publish an older missing version of ${pkg.name}`);
      missing.push(pkg);
    }
  }
  for (const pkg of missing) await publish(pkg);
  return { published: missing.length, skipped: packages.length - missing.length };
}

export function verifyAssets(expected, existing) {
  return expected.filter(asset => {
    const previous = existing.find(item => item.name === asset.name);
    if (!previous) return true;
    if (previous.digest !== `sha256:${asset.sha256}`) throw new Error(`Existing asset ${asset.name} has a different or unavailable digest; refusing to replace it`);
    return false;
  }).map(asset => asset.name);
}

export function selectRelease(pages, tag) {
  return pages.flat().find(release => release.tag_name === tag) ?? null;
}

async function output(values) {
  const text = Object.entries(values).map(([key, value]) => `${key}=${value}`).join('\n');
  if (process.env.GITHUB_OUTPUT) await appendFile(process.env.GITHUB_OUTPUT, `${text}\n`);
  else console.log(text);
}

function manifestAt(sha, product) {
  const file = manifestPath(product);
  if (!git(['ls-tree', sha, '--', file])) return null;
  return parseVersion(git(['show', `${sha}:${file}`]));
}

async function resolve() {
  if (process.env.GITHUB_REF !== 'refs/heads/master') throw new Error('Run the release coordinator from master');
  const event = JSON.parse(await readFile(process.env.GITHUB_EVENT_PATH, 'utf8'));
  if (process.env.GITHUB_EVENT_NAME === 'workflow_dispatch') {
    const { product, release_tag: tag } = event.inputs;
    const sha = resolveSource(product, tag, null);
    if (manifestAt(sha, product) !== tagVersion(product, tag)) throw new Error('Tag does not match the tagged product manifest');
    await output({ source_sha: sha, cli_tag: product === 'cli' ? tag : '', lsp_tag: product === 'lsp' ? tag : '' });
  } else {
    const sha = validateSha(process.env.GITHUB_SHA);
    const before = validateSha(event.before);
    git(['cat-file', '-e', `${before}^{commit}`]);
    const current = Object.fromEntries(['cli', 'lsp'].map(product => [product, manifestAt(sha, product)]));
    const previous = Object.fromEntries(['cli', 'lsp'].map(product => [product, manifestAt(before, product)]));
    const changed = detectReleases(previous, current);
    for (const product of changed) resolveSource(product, `${product === 'cli' ? 'v' : 'lsp-v'}${current[product]}`, sha);
    await output({ source_sha: sha, cli_tag: changed.includes('cli') ? `v${current.cli}` : '', lsp_tag: changed.includes('lsp') ? `lsp-v${current.lsp}` : '' });
  }
}

async function validate(product, tag, sha) {
  if (git(['rev-parse', 'HEAD']) !== validateSha(sha)) throw new Error('Checkout does not match the requested source SHA');
  if (await checkVersions(process.cwd(), product) !== tagVersion(product, tag)) throw new Error('Tag and package version differ');
  resolveSource(product, tag, sha);
}

async function notes(product, tag, sha) {
  const prefix = product === 'cli' ? 'v' : 'lsp-v';
  const previous = git(['tag', '--merged', sha, '--list', `${prefix}[0-9]*`]).split('\n')
    .filter(value => value && value !== tag && (() => { try { return compareVersions(tagVersion(product, value), tagVersion(product, tag)) < 0; } catch { return false; } })())
    .sort((a, b) => compareVersions(tagVersion(product, b), tagVersion(product, a)))[0];
  const paths = product === 'cli' ? ['src/**', 'packages/**', 'Cargo.*', 'scripts/**', '.github/**']
    : ['lsp/**', 'src/compiler/**', 'src/analysis.rs', 'src/execution_context.rs', 'src/project/**', 'src/lib.rs', 'Cargo.*', 'scripts/**', '.github/**'];
  run('git', ['cliff', ...(previous ? [`${previous}..${sha}`] : [sha]), '--tag', tag, '--tag-pattern', `^${prefix}[0-9]+[.][0-9]+[.][0-9]+$`, '--config', 'cliff.toml', ...paths.flatMap(value => ['--include-path', value]), '--output', 'target/release-prepared/notes.md']);
}

const digest = (buffer, algorithm = 'sha256', encoding = 'hex') => createHash(algorithm).update(buffer).digest(encoding);

async function createManifest(directory, product, tag, sha) {
  const assets = [];
  for (const name of (await readdir(path.join(directory, 'assets'))).sort()) {
    if (name === 'SHA256SUMS') continue;
    assets.push({ name, sha256: digest(await readFile(path.join(directory, 'assets', name))) });
  }
  const expected = product === 'cli' ? 5 : 10;
  if (assets.length !== expected) throw new Error(`Expected ${expected} ${product} assets, got ${assets.length}`);
  const sums = assets.map(asset => `${asset.sha256}  ${asset.name}\n`).join('');
  await writeFile(path.join(directory, 'assets/SHA256SUMS'), sums);
  assets.push({ name: 'SHA256SUMS', sha256: digest(Buffer.from(sums)) });
  const packages = [];
  if (product === 'cli') {
    for (const name of (await readdir(path.join(directory, 'npm'))).sort()) {
      if (!name.endsWith('.tgz')) continue;
      const file = path.join(directory, 'npm', name);
      const metadata = JSON.parse(run('tar', ['-xOf', file, 'package/package.json']));
      packages.push({ name: metadata.name, version: metadata.version, file: `npm/${name}`, integrity: `sha512-${digest(await readFile(file), 'sha512', 'base64')}` });
    }
    if (packages.length !== 6) throw new Error('Expected six prepared npm packages');
    packages.sort((a, b) => Number(a.name === '@luatlang/cli') - Number(b.name === '@luatlang/cli') || a.name.localeCompare(b.name));
  }
  await writeFile(path.join(directory, 'release.json'), `${JSON.stringify({ product, tag, source_sha: sha, run_id: process.env.GITHUB_RUN_ID, assets, packages }, null, 2)}\n`);
}

function ghJson(args, allowMissing = false) {
  try { return JSON.parse(run('gh', ['api', ...args])); }
  catch (error) { if (allowMissing && /\(HTTP 404\)/.test(error.message)) return null; throw error; }
}

async function registryJson(name, suffix) {
  const response = await fetch(`https://registry.npmjs.org/${encodeURIComponent(name)}/${encodeURIComponent(suffix)}`);
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`npm registry HTTP ${response.status} for ${name}`);
  return response.json();
}

async function publish(directory, product, tag, sha) {
  const repository = process.env.GITHUB_REPOSITORY;
  const lookup = () => selectRelease(ghJson(['--paginate', '--slurp', `repos/${repository}/releases?per_page=100`]), tag);
  let release = lookup();
  const originalRun = release?.body?.match(/<!-- luat-release-run:(\d+) -->/)?.[1];
  if (originalRun && originalRun !== process.env.GITHUB_RUN_ID) {
    directory = path.resolve('target/release-recovered');
    await mkdir(directory, { recursive: true });
    run('gh', ['run', 'download', originalRun, '--repo', repository, '--name', `${product}-prepared-${sha}`, '--dir', directory]);
  }
  const state = JSON.parse(await readFile(path.join(directory, 'release.json'), 'utf8'));
  if (state.product !== product || state.tag !== tag || state.source_sha !== sha) throw new Error('Prepared artifact belongs to another release/source');
  for (const asset of state.assets) {
    if (path.basename(asset.name) !== asset.name || digest(await readFile(path.join(directory, 'assets', asset.name))) !== asset.sha256) throw new Error('Prepared asset checksum mismatch');
  }
  for (const pkg of state.packages) {
    if (!/^npm\/[a-zA-Z0-9._-]+\.tgz$/.test(pkg.file) || pkg.version !== tagVersion(product, tag)
      || !/^(@luatlang\/cli|luat-(win32|linux|darwin)-(x64|arm64))$/.test(pkg.name)
      || `sha512-${digest(await readFile(path.join(directory, pkg.file)), 'sha512', 'base64')}` !== pkg.integrity) throw new Error('Prepared npm package mismatch');
  }
  if (!release) {
    const remoteTag = ghJson([`repos/${repository}/git/ref/tags/${encodeURIComponent(tag)}`], true);
    if (!remoteTag) ghJson([`repos/${repository}/git/refs`, '--method', 'POST', '-f', `ref=refs/tags/${tag}`, '-f', `sha=${sha}`]);
    else resolveSource(product, tag, sha);
    const title = `${product === 'cli' ? 'LuaT CLI' : 'LuaT Language Server'} ${tagVersion(product, tag)}`;
    const body = `${await readFile(path.join(directory, 'notes.md'), 'utf8')}\n<!-- luat-release-run:${process.env.GITHUB_RUN_ID} -->\n`;
    const notesFile = path.join(directory, 'publish-notes.md');
    await writeFile(notesFile, body);
    run('gh', ['release', 'create', tag, '--repo', repository, '--target', sha, '--draft', '--title', title, '--notes-file', notesFile]);
    release = lookup();
    if (!release) throw new Error('Created draft release was not found');
  }
  // Older assets may not have server-side digests; hash their downloaded bytes.
  const existing = [];
  for (const asset of ghJson([`repos/${repository}/releases/${release.id}/assets?per_page=100`])) {
    if (!state.assets.some(item => item.name === asset.name)) continue;
    if (!asset.digest) {
      const folder = path.resolve('target/release-existing');
      await mkdir(folder, { recursive: true });
      run('gh', ['release', 'download', tag, '--repo', repository, '--pattern', asset.name, '--dir', folder]);
      asset.digest = `sha256:${digest(await readFile(path.join(folder, asset.name)))}`;
    }
    existing.push(asset);
  }
  const missing = verifyAssets(state.assets, existing);
  if (!release.draft && missing.length) throw new Error('Published release is incomplete; restore its original assets explicitly instead of replacing a public release');
  for (const name of missing) run('gh', ['release', 'upload', tag, path.join(directory, 'assets', name), '--repo', repository]);
  if (product === 'cli') {
    const result = await publishPackages(state.packages, {
      registry: pkg => registryJson(pkg.name, pkg.version), latest: async pkg => (await registryJson(pkg.name, 'latest'))?.version,
      publish: async pkg => { console.log(`Publishing ${pkg.name}@${pkg.version}`); run('npm', ['publish', path.resolve(directory, pkg.file), '--access', 'public', '--provenance', '--tag', 'latest']); },
    });
    console.log(`npm: ${result.published} published, ${result.skipped} already matched`);
  }
  if (release.draft) run('gh', ['release', 'edit', tag, '--repo', repository, '--draft=false', `--latest=${product === 'cli'}`]);
  const summary = `${product}: ${tag} from ${sha}. npm/GitHub publication is not atomic; failed npm publication leaves a recoverable draft.\n`;
  console.log(summary);
  if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, summary);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [command, product = process.env.PRODUCT, tag = process.env.RELEASE_TAG, sha = process.env.SOURCE_SHA] = process.argv.slice(2);
  const directory = path.resolve('target/release-prepared');
  if (command === 'resolve') await resolve();
  else {
    tagVersion(product, tag);
    validateSha(sha);
    if (command === 'validate') await validate(product, tag, sha);
    else if (command === 'notes') await notes(product, tag, sha);
    else if (command === 'manifest') await createManifest(directory, product, tag, sha);
    else if (command === 'publish') await publish(directory, product, tag, sha);
    else throw new Error(`Unknown release command: ${command}`);
  }
}
