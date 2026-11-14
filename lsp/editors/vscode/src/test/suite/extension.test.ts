import * as assert from 'node:assert/strict';
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import * as vscode from 'vscode';

suite('LuaT extension host', () => {
	const workspace = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
	const mainFile = path.join(workspace ?? '', 'main.luat');
	const partialFile = path.join(workspace ?? '', 'partial.luat');
	let mainDocument: vscode.TextDocument;
	let partialDocument: vscode.TextDocument;

	setup(async () => {
		assert.ok(workspace, 'test workspace was not opened');
		await vscode.workspace.getConfiguration('luat').update('server.path', process.env.LUAT_LSP_SERVER, vscode.ConfigurationTarget.Global);
		await fs.mkdir(workspace, { recursive: true });
		await fs.writeFile(mainFile, "local value: number = 'bad'\nprint(value)\nGetEntityCoords(1)\n");
		await fs.writeFile(partialFile, 'GetEnt');
		mainDocument = await vscode.workspace.openTextDocument(mainFile);
		partialDocument = await vscode.workspace.openTextDocument(partialFile);
		await vscode.window.showTextDocument(mainDocument);
		await vscode.window.showTextDocument(partialDocument);
		await waitFor(() => vscode.languages.getDiagnostics(mainDocument.uri).some(item => item.code === 'LT2001'));
	});

	teardown(async () => {
		await vscode.workspace.getConfiguration('luat').update('server.path', undefined, vscode.ConfigurationTarget.Global);
	});

	test('serves semantic symbol tokens', async () => {
        const result = await vscode.commands.executeCommand<vscode.SemanticTokens>('vscode.provideDocumentSemanticTokens', mainDocument.uri);
        assert.ok(result && result.data.length > 0);
    });

	test('provides Markdown, unused fixes, symbols and configurable inlay hints', async () => {
		const file = path.join(workspace!, 'assistance.luat');
		await fs.writeFile(file, "---[Reference](https://www.lua.org/manual/5.4/)\n---@param value Text.\nlocal function demo(value: string, unused: number): string return value end\nlocal count = 1\nprint(demo('x', 1))\n");
		const document = await vscode.workspace.openTextDocument(file);
		await vscode.window.showTextDocument(document);
		await waitFor(() => vscode.languages.getDiagnostics(document.uri).some(item => item.code === 'LTH1001'));
		const diagnostic = vscode.languages.getDiagnostics(document.uri).find(item => item.code === 'LTH1001')!;
		assert.ok(diagnostic.tags?.includes(vscode.DiagnosticTag.Unnecessary));
		const actions = await vscode.commands.executeCommand<vscode.CodeAction[]>('vscode.executeCodeActionProvider', document.uri, diagnostic.range, vscode.CodeActionKind.QuickFix.value);
		assert.ok(actions.some(action => action.edit));
		const symbols = await vscode.commands.executeCommand<vscode.DocumentSymbol[]>('vscode.executeDocumentSymbolProvider', document.uri);
		assert.ok(symbols.some(symbol => symbol.name === 'demo'));
		const hover = await vscode.commands.executeCommand<vscode.Hover[]>('vscode.executeHoverProvider', document.uri, new vscode.Position(4, 8));
		assert.ok(hover.some(item => item.contents.some(content => typeof content !== 'string' && 'value' in content && content.value.includes('[Reference](https://'))));
		const configuration = vscode.workspace.getConfiguration('luat');
		const oldHints = configuration.inspect<boolean>('inlayHints.parameterNames.enabled')?.workspaceValue;
		const oldUnused = configuration.inspect<boolean>('diagnostics.unusedParameters')?.workspaceValue;
		try {
			await configuration.update('inlayHints.parameterNames.enabled', true, vscode.ConfigurationTarget.Workspace);
			await configuration.update('diagnostics.unusedParameters', false, vscode.ConfigurationTarget.Workspace);
			await waitFor(() => !vscode.languages.getDiagnostics(document.uri).some(item => item.code === 'LTH1001'));
			const hints = await vscode.commands.executeCommand<vscode.InlayHint[]>('vscode.executeInlayHintProvider', document.uri, new vscode.Range(0, 0, 5, 0));
			assert.ok(hints.some(hint => hint.label === 'value:'));
		} finally {
			await configuration.update('inlayHints.parameterNames.enabled', oldHints, vscode.ConfigurationTarget.Workspace);
			await configuration.update('diagnostics.unusedParameters', oldUnused, vscode.ConfigurationTarget.Workspace);
		}
	});

	test('publishes diagnostics and responds to hover, completion, and rename', async () => {
		assert.ok(vscode.languages.getDiagnostics(mainDocument.uri).some(item => item.code === 'LT2001'));

		const hoverOffset = mainDocument.getText().indexOf('GetEntityCoords');
		const hover = await vscode.commands.executeCommand<vscode.Hover[]>(
			'vscode.executeHoverProvider', mainDocument.uri, mainDocument.positionAt(hoverOffset),
		);
		assert.ok(hover.length > 0);

		const completion = await vscode.commands.executeCommand<vscode.CompletionList>(
			'vscode.executeCompletionItemProvider', partialDocument.uri, partialDocument.positionAt(partialDocument.getText().length),
		);
		assert.ok(completion.items.some(item => item.label === 'GetEntityCoords'));

		const renameOffset = mainDocument.getText().indexOf('value');
		const edit = await vscode.commands.executeCommand<vscode.WorkspaceEdit>(
			'vscode.executeDocumentRenameProvider', mainDocument.uri, mainDocument.positionAt(renameOffset), 'amount',
		);
		assert.ok(edit.entries().some(([uri, edits]) => uri.toString() === mainDocument.uri.toString() && edits.length === 2));
	});

	test('completes runtime members and renders documented function signatures', async () => {
		const runtimeFile = path.join(workspace!, 'runtime.luat');
		await fs.writeFile(runtimeFile, '');
		const document = await vscode.workspace.openTextDocument(runtimeFile);
		const editor = await vscode.window.showTextDocument(document);
		await editor.edit(edit => edit.insert(new vscode.Position(0, 0), 'SendNUIMessage({})\njson.encode({})\nCitizen.CreateThread(function() end)\njson.en\nCit'));
		for (const [line, name] of [[0, 'SendNUIMessage'], [1, 'json.encode'], [2, 'Citizen.CreateThread']] as const) {
			const hover = await vscode.commands.executeCommand<vscode.Hover[]>('vscode.executeHoverProvider', document.uri, new vscode.Position(line, name.indexOf('.') + 1));
			const markdown = hover.flatMap(item => item.contents).map(item => typeof item === 'string' ? item : 'value' in item ? item.value : '').join('\n');
			assert.ok(markdown.includes('```luat\nfunction ' + name), markdown);
			assert.ok(!markdown.includes('**Documentation**'), markdown);
		}
		const members = await vscode.commands.executeCommand<vscode.CompletionList>('vscode.executeCompletionItemProvider', document.uri, new vscode.Position(3, 7));
		assert.ok(members.items.some(item => item.label === 'encode' && item.kind === vscode.CompletionItemKind.Function));
		const globals = await vscode.commands.executeCommand<vscode.CompletionList>('vscode.executeCompletionItemProvider', document.uri, new vscode.Position(4, 3));
		assert.ok(globals.items.some(item => item.label === 'Citizen'));
	});
});

async function waitFor(predicate: () => boolean): Promise<void> {
	const deadline = Date.now() + 10_000;
	while (!predicate()) {
		if (Date.now() >= deadline) throw new Error('LuaT server did not publish diagnostics in time');
		await new Promise(resolve => setTimeout(resolve, 100));
	}
}
