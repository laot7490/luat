<center>
  <picture>
    <img src="logos/banner.png" alt="LuaT" style="margin-bottom: 1rem;" />
  </picture>

  <p>
    <a href="https://www.npmjs.com/package/@luatlang/cli"><img src="https://img.shields.io/npm/v/@luatlang/cli?style=flat-square&logo=npm&logoColor=white&label=npm&color=9D82F1" alt="npm version" /></a>
    <a href="https://nodejs.org/"><img src="https://img.shields.io/badge/Node.js-22%2B-339933?style=flat-square&logo=nodedotjs&logoColor=white" alt="Node.js 22 or later" /></a>
    <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue?style=flat-square" alt="MIT license" /></a>
  </p>

  <p><a href="#why-luat">Features</a> · <a href="#get-started">Get started</a> · <a href="#commands">Commands</a> · <a href="#community-packages">Packages</a> · <a href="#example-syntax">Example syntax</a> · <a href="#local-imports">Local imports</a> · <a href="#language-server">Language server</a> · <a href="#project-settings">Configuration</a>
</center>

## Why LuaT?

- **Type-Safety**: Add types to Lua and catch mismatched values, missing fields, and other issues before they reach your resource. LuaT checks what it can infer from your code and declarations, so you can add type information where it helps most.

- **Made for FiveM**: Build shared, client, and server scripts into a resource, with an `fxmanifest.lua` generated for you.

- **Less Boilerplate**: Use type aliases, optional fields, optional chaining, nullish coalescing, conditional expressions, and string interpolation to write common patterns clearly.

- **Editor Support**: Get diagnostics, completions, hover documentation, and navigation in VS Code or another LSP-compatible editor.

- **Simple Project Workflow**: Check, build, and watch your resource from the CLI. Local imports let you split code into modules, and community type packages add declarations for libraries and resources.

- **Fast Builds**: LuaT is written in Rust and designed to compile your project quickly.

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

| Command                    | What it does                                                                  |
| -------------------------- | ----------------------------------------------------------------------------- |
| `luat create [directory]`  | Creates starter project files in the current or a named directory.            |
| `luat check`               | Checks the project's scripts and reports issues it finds.                     |
| `luat build`               | Builds the scripts and FiveM resource manifest.                               |
| `luat watch [directory]`   | Builds the project and rebuilds it when source or configuration files change. |
| `luat add <package>`       | Adds a community package to the project.                                      |
| `luat install`             | Restores the package versions recorded in `luat.toml`.                        |
| `luat update [package]`    | Updates dependencies within their declared version ranges.                    |
| `luat package init <name>` | Creates a package with `luat-package.toml`.                                   |
| `luat package check`       | Checks a package without installing dependencies.                             |
| `luat remove <package>`    | Removes a package from the project.                                           |

Use `--project <directory>` with `check` or `build` to select a project in another directory. For example:

## Project settings

LuaT project settings are stored in `luat.toml`. The settings describe the resource and its shared, client, and server scripts. See [`luat.toml.example`](luat.toml.example) for the available options.

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

## Local imports

Use `import()` to share a local `.luat` module.
Local imports works flawlessly with the FiveM's Asset Escrow system unlike traditional require systems.

```lua
-- src/shared/settings.luat
return { greeting = 'Hello' }
```

Another file can import like this:

```lua
-- src/client/main.luat
local settings = import('@/shared/settings')
print(settings.greeting)
-- Output: Hello
```

## Community packages

The LuaT community registry supports LuaT/Lua modules and optional declarations. Packages declare shared, client and server contexts in `luat-package.toml`; projects use `luat.toml`. Install one from your project folder:

```bash
luat add @types/example
```

You can install community packages via CLI:

```bash
luat add @types/example@1.0.0
luat install
luat remove @types/example
```

The community registry is [luat-packages](https://github.com/laot7490/luat-packages). See the registry’s [contribution guide](https://github.com/laot7490/luat-packages/blob/main/CONTRIBUTING.md) for the walkthrough.

## Language server

LuaT's language server provides editor features for `.luat` and `.d.luat` files, including diagnostics, completion, and navigation.

### Use it in Visual Studio Code

Install the [LuaT Language Server extension from the Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=luat.luat-language-server). See the [VS Code setup guide](lsp/editors/vscode/README.md) for details.

### Use it in another editor

Editors with Language Server Protocol (LSP) support can connect to `luat-lsp --stdio`. See the [language server guide](lsp/README.md) for setup and release details.

## LICENSE

[MIT](LICENSE)
