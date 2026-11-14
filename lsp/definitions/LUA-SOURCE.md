## Lua standard library

- Hand-maintained `lua-shared.d.luat` and `lua-server.d.luat`, based on the [Lua 5.4 manual](https://www.lua.org/manual/5.4/manual.html).
- CfxLua runtime commit: `5412ed86f9eff1b81d2afb10cff85153b043d3b8`; [library loading and global replacements](https://github.com/citizenfx/fivem/blob/5412ed86f9eff1b81d2afb10cff85153b043d3b8/code/components/citizen-scripting-lua/src/LuaScriptRuntime.cpp).
- Embedded Lua commit: `4b086dc082da96a8e21aaa38ef7c0f4a7697f2a4`; [sandbox debug exports](https://github.com/citizenfx/lua/blob/4b086dc082da96a8e21aaa38ef7c0f4a7697f2a4/ldblib.c). Only getinfo, getmetatable, setmetatable, getupvalue and traceback are exposed.
- Server io/os exports follow LuaIO.cpp and LuaOS.cpp in the same CfxLua commit. No package library, dofile, loadfile, io.input/output/read or os.exit; CfxLua replaces require and print.
- [Sandbox restrictions](https://docs.fivem.net/docs/developers/sandbox/) are descriptions, not a claim that filesystem operations succeed. Additional supported io.readdir/os timing functions are included.
- Scope: Lua standard APIs and the documented io/os additions; GLM extensions remain owned by the existing runtime declarations.
- Approximation: dependent generic returns, iterator packs, format-dependent results and vararg return packs use any where the current declaration syntax cannot express them. No compiler type-system changes.
- Inventory: every function line in both Lua declaration files is checked against the builtin index by Rust tests. Type aliases hold namespace constants and file/directory methods.
