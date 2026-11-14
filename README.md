<div align="center">
  <img src="logos/logo.png" alt="LuaT" width="100" />

  <h1>Luat</h1>
  <p><strong>Catch type mistakes in Lua scripts and build FiveM resources.</strong></p>

  <p>
    <a href="https://www.npmjs.com/package/@luatlang/cli"><img src="https://img.shields.io/npm/v/@luatlang/cli?style=flat-square&logo=npm&logoColor=white&label=npm&color=9D82F1" alt="npm version" /></a>
    <a href="https://nodejs.org/"><img src="https://img.shields.io/badge/Node.js-22%2B-339933?style=flat-square&logo=nodedotjs&logoColor=white" alt="Node.js 22 or later" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT license" /></a>
  </p>

  <p><a href="#what-luat-does">Features</a> · <a href="#get-started">Get started</a> · <a href="#commands">Commands</a> · <a href="#example-syntax">Example syntax</a> · <a href="#local-imports">Local imports</a> · <a href="#language-server">Language server</a> · <a href="#project-settings">Configuration</a>
</div>

## What LuaT does

- Checks typed Lua scripts and points out issues it can identify from the information available.
- Builds CfxLua scripts and a FiveM resource manifest from your project.
- Provides commands to create a project, check it, build it, and rebuild it as you edit.

> [!NOTE]
> LuaT is in early development. You may encounter bugs or unexpected behavior. If you experience any issues, please [open an issue](https://github.com/laot7490/luat/issues) on GitHub.

## Get started

You need Node.js 22 or later. Install LuaT globally, then create starter files in your FiveM resource directory:

```bash
npm install -g @luatlang/cli
luat create
```

Then check and build the resource:

```bash
luat check
luat build
```

The build writes the compiled scripts to `dist/` and creates `fxmanifest.lua` in the project directory.

## Commands

| Command                   | What it does                                                                  |
| ------------------------- | ----------------------------------------------------------------------------- |
| `luat create [directory]` | Creates starter project files in the current or a named directory.            |
| `luat check`              | Checks the project's scripts and reports issues it finds.                     |
| `luat build`              | Builds the scripts and FiveM resource manifest.                               |
| `luat watch [directory]`  | Builds the project and rebuilds it when source or configuration files change. |

Use `--project <directory>` with `check` or `build` to select a project in another directory. For example:

```bash
luat check --project resources/my-resource
luat build --project resources/my-resource
```

## Project settings

LuaT reads project settings from `luat.toml`. The settings describe the resource and its shared, client, and server scripts. See [`luat.toml.example`](luat.toml.example) for the available options.

## Example syntax

LuaT supports type annotations and a few concise ways to write common Lua expressions:

```lua
typedef Job = 'police' | 'medic'
typedef Player = { name: string, job: Job, callsign?: string }

const player: Player = { name = 'Ada', job = 'police' }

-- Input: read the name only when player exists; use the fallback only for nil.
const name = player?.name ?? 'Guest'
-- Output: 'Ada'

-- Input: choose one value from the condition.
const status = if player.job == 'police' then 'On duty' else 'Available'
-- Output: 'On duty'

-- Input: insert values into a string.
local message = `Welcome, {name}! You are {status}.`
-- Output: 'Welcome, Ada! You are On duty.'
```

`?.` safely accesses a property when the value before it may be `nil`. `??` uses its fallback only when the value on the left is `nil`.
`const` prevents reassignment of the variable; table fields such as `player.name` can still be changed.

## Local imports

Use `import()` to share a local `.luat` module. For example, a module can return settings:

```lua
-- src/shared/settings.luat
return { greeting = 'Hello' }
```

Another script can import and use them:

```lua
-- src/client/main.luat
local settings = import('@/shared/settings')
print(settings.greeting)
-- Output: Hello
```

Imported modules are included in the resource build automatically. The `@/` path starts from the project's `src/` directory.
Imports are included in the built resource and work with Asset Escrow unlike the FiveM's require system.

## Language server

LuaT includes a language server for `.luat` and `.d.luat` files. The VS Code extension and setup instructions are in [`lsp/editors/vscode`](lsp/editors/vscode/README.md); general LSP clients can run `luat-lsp --stdio`. See the [server guide](lsp/README.md) for build and release archive details.
