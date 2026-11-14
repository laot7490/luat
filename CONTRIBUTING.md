# Contributing to LuaT

Thank you for your interest in contributing to LuaT. LuaT is an early-stage Rust compiler and CLI for typed Lua targeting CfxLua, and contributions of all sizes are welcome.

## Before you start

- For bug reports, open a [GitHub issue](https://github.com/laot7490/luat/issues).
- Please discuss substantial changes and new features in an issue before opening a pull request. Small fixes and documentation improvements can go directly to a pull request.
- Keep discussions respectful and constructive. Be considerate of other contributors and their perspectives.

## Report a bug

Include enough information for someone else to reproduce the problem:

- LuaT version and operating system
- Steps to reproduce the issue
- What you expected to happen and what happened instead
- A small `.luat` example or project configuration, when relevant

Remove secrets and unrelated project data from examples before sharing them.

## Set up the project

Install the stable Rust toolchain with the `rustfmt` and `clippy` components. Node.js 22 or later is needed for the Node.js-based workspace and VS Code extension tasks. Run Rust commands from the repository root. The VS Code extension has its own npm dependencies; run `npm ci` from `lsp/editors/vscode` when working on it.

## Make and test your change

- Follow the existing Rust style and module structure. Avoid adding dependencies when the existing Rust or workspace tools can do the job.
- Add regression tests for new or changed behavior. Put Rust tests beside the relevant code using the existing module test layout. Put JavaScript tests in a `test.mjs` file alongside the module they cover.
- Keep temporary projects and generated test output under `target/tests/`.
- For Rust changes, run `cargo fmt --all --check`, `cargo test --workspace --locked`, and `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- For changes to release/version scripts, run the relevant Node tests. For VS Code extension changes, run `npm run typecheck` and `npm test` from `lsp/editors/vscode`.
- For CLI behavior changes, check the sample project with `cargo run -- check --project example` and `cargo run -- build --project example` where relevant.

## Open a pull request

Fork the repository, create a branch for your change, and open a pull request against the repository's default branch. In the pull request description:

- Summarize the user-visible change.
- Link a related issue, if there is one.
- List the checks you ran and their results.
- Include CLI output or screenshots when they help explain the change.

Use a concise Conventional Commit subject in English, for example `fix: handle nested imports` or `feat: add type alias support`.
