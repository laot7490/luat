<div align="center">
  <img src="../logos/logo.png" alt="LuaT logo" width="96" />
  <h1>LuaT Language Server</h1>
  <p><strong>Editor tools for typed Lua and FiveM.</strong></p>
  <p>Diagnostics · Suggestions · Documentation · Navigation</p>
</div>

LuaT helps you write `.luat` and `.d.luat` files with live feedback, code suggestions, type and documentation on hover, and navigation through your project. FiveM definitions are included for client, server, and shared code.

Choose how to use it:

- **Visual Studio Code:** install the extension. The server is included and starts automatically.
- **Other editors:** download and run the language server manually, then connect your editor to it.

## Visual Studio Code extension

Install [LuaT Language Server from the Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=luat.luat-language-server), open your FiveM resource folder, and start editing a `.luat` file. The extension handles the server setup for you; no separate download is needed.

If you need to install the extension without Marketplace access, download the matching `luat-vscode-<platform>.vsix` from the [LuaT Language Server releases](https://github.com/laot7490/luat/releases?q=lsp-v). In VS Code, run **Extensions: Install from VSIX...** and select the downloaded file.

See the [VS Code extension guide](editors/vscode/README.md) for the features it provides and its settings.

## Manual setup for other editors

1. Download the `luat-lsp-<platform>.tar.gz` archive for your system from the [LuaT Language Server releases](https://github.com/laot7490/luat/releases?q=lsp-v).
2. Extract it and keep the included `definitions/` folder beside the `luat-lsp` executable.
3. In your editor's language server settings, set the executable path to `luat-lsp` (`luat-lsp.exe` on Windows) and add the argument `--stdio`.
4. Open the folder containing your `luat.toml` and edit a `.luat` or `.d.luat` file.

The release archives are available for Windows x64, Linux x64 and ARM64, and macOS Intel and Apple Silicon. Your editor must support the Language Server Protocol (LSP).

## Development

To build or test the server from the repository root, install the Rust toolchain and run:

```sh
cargo build --release -p luat-lsp
cargo test -p luat-lsp
```

The executable is written to `target/release/luat-lsp` (`luat-lsp.exe` on Windows). To try your local build in VS Code, set `luat.server.path` to the executable's full path.

> [!NOTE]
> The language server supports LuaT files; it does not provide a standalone Lua 5.4 mode or code formatting. Use the [LuaT CLI](../README.md#commands) to check and build your resource.
