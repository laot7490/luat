<div align="center">
  <img src="../logos/logo.png" alt="LuaT logo" width="96" />
  <h1>LuaT Language Server</h1>
  <p><strong>Typed Lua tooling for your FiveM editor workflow.</strong></p>
  <p>Diagnostics · Completion · Navigation · Rename</p>
  <p><a href="#features">Features</a> · <a href="#get-started">Get started</a> · <a href="#other-editors">Other editors</a> · <a href="#development">Development</a></p>
</div>

`luat-lsp` brings [LuaT](../README.md) analysis to editors through the Language Server Protocol. It supports `.luat` source files and `.d.luat` declarations, with bundled FiveM definitions for built-in navigation.

## Features

| Feature | What you get |
| --- | --- |
| Diagnostics | Compiler diagnostics as you edit, including type errors. |
| Completion | Suggestions for symbols and members. |
| Hover | Type information and available documentation. |
| Signature help | Function signatures and parameter information while writing calls. |
| Navigation | Go to definition and find references. |
| Rename | Symbol renaming when the server can resolve the edit safely. |
| FiveM definitions | Client, server, and shared declarations shipped with the server. |

> [!NOTE]
> LuaT is in early development. The server does not provide a standalone Lua 5.4 language mode or formatting.

## Get started

### Visual Studio Code

Use the [LuaT extension](editors/vscode/README.md) with VS Code 1.90 or later. Platform VSIX packages bundle the server and FiveM declarations, so normal use needs no separate Node.js or Rust installation and no network connection.

1. Download the matching `luat-vscode-<platform>.vsix` from a [LuaT Language Server release](https://github.com/laot7490/luat/releases?q=lsp-v) tagged `lsp-v...`.
2. Run **Extensions: Install from VSIX...** in the Command Palette and select the file.
3. Open your resource folder and a `.luat` file. The server starts automatically.

Keep your resource's `luat.toml` in the project folder so analysis can use its project configuration.

### Standalone server

Extract a `luat-lsp-<platform>.tar.gz` archive from a Language Server release tagged `lsp-v...`. Keep its `definitions/` directory beside the executable, then configure your editor to launch:

```sh
luat-lsp --stdio
```

Release targets are Windows x64, Linux x64 / ARM64, and macOS Intel / Apple Silicon.

Language Server starts at version 0.1.0 independently of the compiler/CLI. The bundled VS Code extension follows the server version. Use `npm run prepare:lsp:patch` from the repository root to prepare a server/editor update; see the [release guide](../.github/WORKFLOWS.md).

## Other editors

Any editor with an LSP client can launch the server over standard input and output:

| Setting | Value |
| --- | --- |
| Executable | Absolute path to `luat-lsp` (`luat-lsp.exe` on Windows) |
| Arguments | `--stdio` |
| File patterns | `*.luat`, including `*.d.luat` |
| Project configuration | `luat.toml` |

The server does not compile or run your resource. Use the [LuaT CLI](../README.md#commands) to check and build it.

## Development

From the repository root, with the Rust toolchain installed:

```sh
cargo build --release -p luat-lsp
cargo test -p luat-lsp
```

The executable is written to `target/release/luat-lsp` (`luat-lsp.exe` on Windows). In VS Code, point `luat.server.path` at that executable to use your local build.

See the [definition source notes](definitions/SOURCE.md) for the bundled FiveM declarations.

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
