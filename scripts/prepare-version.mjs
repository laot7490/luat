import { readFile, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';

const bump = process.argv[2];
if (!['major', 'minor', 'patch'].includes(bump)) throw new Error('Usage: node scripts/prepare-version.mjs <major|minor|patch>');

const path = new URL('../Cargo.toml', import.meta.url);
const cargo = await readFile(path, 'utf8');
const match = cargo.match(/^(version\s*=\s*")(\d+)\.(\d+)\.(\d+)("\s*)$/m);
if (!match) throw new Error('Cargo.toml must have a numeric major.minor.patch version');

let [, prefix, major, minor, patch, suffix] = match;
major = Number(major);
minor = Number(minor);
patch = Number(patch);
if (bump === 'major') [major, minor, patch] = [major + 1, 0, 0];
if (bump === 'minor') [minor, patch] = [minor + 1, 0];
if (bump === 'patch') patch += 1;

await writeFile(path, cargo.replace(match[0], `${prefix}${major}.${minor}.${patch}${suffix}`));
const result = spawnSync('cargo', ['check', '--offline'], { stdio: 'inherit', shell: process.platform === 'win32' });
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
