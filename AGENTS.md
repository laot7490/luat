# Repository Guidelines

## Project Structure

LuaT is a Rust compiler and CLI for typed Lua targeting CfxLua. Core code lives in `src/`: `compiler/` handles parsing, checking, and lowering; `project/` handles configuration, imports, manifests, and builds; `cli/` contains command behavior and diagnostics. Rust unit tests are colocated with modules. `packages/` contains npm platform packages and the CLI wrapper; `scripts/` contains workspace maintenance scripts.

## Build, Test, and Development

- `cargo build` builds the Rust library and CLI.
- `cargo test` runs Rust unit and integration tests.
- `cargo fmt --check` checks Rust formatting; run `cargo fmt` to apply it.
- `cargo run -- check --project example` checks the sample resource with the local CLI.
- `cargo run -- build --project example` builds the sample resource.

Run Rust commands from the repository root. Use Cargo for compiler checks.

Use only `npm` for package manager. Do not use any other package manager.

Keep the repository root limited to its existing project files. Create a new root-level file or directory only when the task explicitly requires it; put source in the existing project structure and temporary or generated files under `target/`.

## Coding Style and Naming

Use standard Rust formatting (four-space indentation) and keep modules focused by compiler, project, and CLI responsibilities. Use `snake_case` for Rust modules, functions, and variables; use descriptive test names that state the behavior under test. Keep fixture paths and Lua source representative of real project layouts. Avoid adding dependencies for functionality available in the existing Rust or workspace dependencies.

## Commits and Pull Requests

Use Conventional Commit subjects, such as `fix: handle nested imports` or `feat: add type alias support`. The repository has no commit history yet to establish additional local conventions. Pull requests should explain the user-visible change, list relevant checks run, link related issues when applicable, and include CLI output or screenshots only when they clarify a user-facing change.

## Readme Edits

Do not edit, modify or delete README.md files unless user requests.
