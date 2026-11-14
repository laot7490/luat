import * as path from "node:path";
import * as vscode from "vscode";
import { LanguageClient, LanguageClientOptions, ServerOptions, TransportKind } from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
	const output = vscode.window.createOutputChannel("LuaT Language Server");
	const configuredPath = vscode.workspace.getConfiguration("luat").get<string>("server.path", "").trim();
	const command = configuredPath || packagedServer(context);
	const serverOptions: ServerOptions = {
		command,
		args: ["--stdio"],
		transport: TransportKind.stdio,
	};
	const watcher = vscode.workspace.createFileSystemWatcher("**/{*.luat,luat.toml}");
	const clientOptions: LanguageClientOptions = {
		documentSelector: [{ scheme: "file", language: "luat" }],
		synchronize: { fileEvents: watcher, configurationSection: "luat" },
        initializationOptions: vscode.workspace.getConfiguration("luat"),
		outputChannel: output,
	};
	client = new LanguageClient("luat", "LuaT Language Server", serverOptions, clientOptions);
	context.subscriptions.push(output, watcher, client);
	context.subscriptions.push(
		vscode.commands.registerCommand("luat.restartLanguageServer", async () => {
			await client?.restart();
		}),
	);
	context.subscriptions.push(vscode.commands.registerCommand("luat.showOutput", () => output.show()));
	try {
		await client.start();
	} catch (error) {
		output.appendLine(`Could not start ${command}: ${String(error)}`);
		void vscode.window.showErrorMessage(
			"LuaT language server could not start. Check the LuaT output channel or luat.server.path.",
		);
	}
}

export async function deactivate(): Promise<void> {
	await client?.stop();
	client = undefined;
}

function packagedServer(context: vscode.ExtensionContext): string {
	const platform = `${process.platform}-${process.arch}`;
	const executable = process.platform === "win32" ? "luat-lsp.exe" : "luat-lsp";
	return path.join(context.extensionPath, "server", platform, executable);
}
