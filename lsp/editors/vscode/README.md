<div align="center">
  <img src="./images/logo.png" alt="LuaT logo" width="96" />
  <h1>LuaT for Visual Studio Code</h1>
  <p><strong>Typed Lua support for your FiveM projects.</strong></p>
  <p>Catch mistakes · Explore APIs · Write code with confidence</p>
</div>

LuaT brings its language tools into VS Code, so you can work on `.luat` and `.d.luat` files without leaving your editor. It includes FiveM definitions for client, server, and shared code.

## What you get

- **Catch problems as you type.** See compiler and type errors in the file where they occur.
- **Write code faster.** Get suggestions for variables, types, functions, and FiveM APIs, plus function help while entering arguments.
- **Understand unfamiliar code.** Hover over a symbol to see its type and documentation.
- **Move around your project.** Jump to definitions, find references, and rename symbols.
- **Read LuaT at a glance.** Syntax colors distinguish types, functions, properties, and constants.
- **Keep your code tidy.** Spot unused local values and use a quick fix to mark intentionally unused names.

## Get started

Install [LuaT from the Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=luat.luat-language-server), then open your FiveM resource folder in VS Code. Open a `.luat` file and LuaT starts automatically.

No separate server download or Rust installation is needed. Keep `luat.toml` in your project folder to let LuaT use your resource settings.

## Optional settings

LuaT works with its defaults. You can turn unused-name hints off or enable parameter and inferred-type hints in VS Code settings. To use a locally built server, set `luat.server.path` to its full file path.

Use **LuaT: Restart Language Server** to restart LuaT, or **LuaT: Show Language Server Output** to see its logs.

## Manual setup

To use LuaT in another editor, follow the [manual language server setup guide](https://github.com/laot7490/luat/blob/master/lsp/README.md).
