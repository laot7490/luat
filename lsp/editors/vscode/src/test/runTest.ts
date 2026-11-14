import * as path from 'node:path';
import { spawnSync } from 'node:child_process';
import { runTests } from '@vscode/test-electron';

async function main(): Promise<void> {
const extensionRoot = path.resolve(__dirname, '../..');
	const repositoryRoot = path.resolve(extensionRoot, '../../..');
	const executable = process.platform === 'win32' ? 'luat-lsp.exe' : 'luat-lsp';
	const server = process.env.LUAT_LSP_SERVER || path.join(repositoryRoot, 'target', 'debug', executable);
	if (!process.env.LUAT_LSP_SERVER) {
		const build = spawnSync('cargo', ['build', '-p', 'luat-lsp'], { cwd: repositoryRoot, stdio: 'inherit' });
		if (build.status !== 0) throw new Error('cargo build -p luat-lsp failed');
	}
	process.env.LUAT_LSP_SERVER = server;
	const workspace = path.join(extensionRoot, 'out', 'test', 'workspace');
	await runTests({
		extensionDevelopmentPath: extensionRoot,
		extensionTestsPath: path.join(extensionRoot, 'out', 'test', 'suite', 'index'),
		launchArgs: [workspace, '--disable-workspace-trust'],
		cachePath: path.join(repositoryRoot, 'target', 'tests', 'vscode-test-cache'),
		version: process.env.VSCODE_VERSION,
	});
}

void main().catch(error => {
	console.error(error);
	process.exit(1);
});
