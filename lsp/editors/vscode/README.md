<div align="center">
  <img src="./logos/logo.png" alt="LuaT logo" width="96" />
  <h1>LuaT for Visual Studio Code</h1>
  <p><strong>Write typed Lua for FiveM with feedback right in your editor.</strong></p>
  <p>Syntax highlighting · IntelliSense · Diagnostics · Navigation</p>
  <p><a href="#features">Features</a> · <a href="#installation">Installation</a> · <a href="#settings">Settings</a> · <a href="#troubleshooting">Troubleshooting</a></p>
</div>

Language support for `.luat` and `.d.luat` files, powered by the LuaT language server. Platform packages include the server and FiveM definitions; normal use requires no separate Node.js or Rust installation and works offline.

## Features

- **Syntax highlighting** for LuaT source and declaration files.
- **Diagnostics** to surface compiler and type errors as you edit.
- **Completion, hover, and signature help** for symbols, types, and function calls.
- **Go to definition and find references** to explore your code.
- **Safe symbol rename** where the server can resolve the change.
- **Bundled FiveM declarations** for client, server, and shared APIs.

## Installation

Requires **VS Code 1.90 or later**.

1. In VS Code, open Extensions and search for **LuaT** by `luat`, or install it from the [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=luat.luat-vscode).
2. Open your resource folder and a `.luat` file.

To install manually, download `luat-vscode-<platform>.vsix` from a [LuaT Language Server release](https://github.com/laot7490/luat/releases?q=lsp-v) tagged `lsp-v...`, then run **Extensions: Install from VSIX...** from the Command Palette.

The language server starts automatically. Keep `luat.toml` in your project folder so it can use your resource configuration.

| Workspace platform  | Package suffix |
| ------------------- | -------------- |
| Windows x64         | `win32-x64`    |
| Linux x64           | `linux-x64`    |
| Linux ARM64         | `linux-arm64`  |
| macOS Intel         | `darwin-x64`   |
| macOS Apple Silicon | `darwin-arm64` |

For remote workspaces, install the extension on the workspace host and use that host's platform package. The server runs where the workspace lives.

## Settings

The bundled server is used by default. To use a local build or a custom executable, set its absolute path in VS Code settings:

```json
{
	"luat.server.path": "D:\\Projects\\LuaT\\target\\release\\luat-lsp.exe"
}
```

On Linux or macOS, use a path such as `/home/you/luat/target/release/luat-lsp`. Reload the VS Code window after changing this setting.

## Commands

Run these from the Command Palette:

| Command                               | Purpose                         |
| ------------------------------------- | ------------------------------- |
| **LuaT: Restart Language Server**     | Restart the current server.     |
| **LuaT: Show Language Server Output** | Open the server output channel. |

## Troubleshooting

If the server cannot start, run **LuaT: Show Language Server Output** and check that your package matches the workspace host. If you set `luat.server.path`, make sure it points to an existing executable that can run on that host.

> [!NOTE]
> LuaT is in early development. This extension does not provide a standalone Lua 5.4 language mode or formatting. Use the [LuaT CLI](https://github.com/laot7490/luat#commands) to build your resource.

For standalone editor setup and building the server from source, see the [language server guide](https://github.com/laot7490/luat/blob/master/lsp/README.md).

## Editor assistance

Hover, completion and signature help render Markdown from `---` documentation comments, including links and code blocks. Types stay in LuaT declarations:

```luat
---Finds a player. [Reference](https://docs.fivem.net/)
---@param id Player identifier.
---@return The matching player.
local function findPlayer(id: number): table
    return {}
end

---@field title Displayed title.
typedef Options = { title: string }
```

Type `---` above a function for a typeless documentation snippet, or `---@` for tag completion. `---@deprecated Description` marks a symbol as deprecated. Record members and fields in typed call arguments provide field suggestions and descriptions. Outline and workspace symbol search use project source symbols.

Unused parameters, local variables and local functions fade by default. Quick Fix can prefix a name with `_` without removing code or changing argument order. The suggestions are editor-only and do not make CLI builds fail.

| Setting | Default |
| --- | --- |
| `luat.diagnostics.unusedParameters` | `true` |
| `luat.diagnostics.unusedLocals` | `true` |
| `luat.inlayHints.parameterNames.enabled` | `false` |
| `luat.inlayHints.variableTypes.enabled` | `false` |

Automatic signature popups use the existing VS Code setting; no separate LuaT toggle is needed:

```json
{ "[luat]": { "editor.parameterHints.enabled": false } }
```

Manual signature help remains available. Bundled Lua 5.4 declarations follow CfxLua availability: `io` and `os` are server-only, and sandbox restrictions are documented. Unknown contexts expose both catalogs; client/shared project contexts hide server-only Lua APIs. All definitions work offline.
