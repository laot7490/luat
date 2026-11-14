# CI and releases

| Workflow | Trigger | Result |
| --- | --- | --- |
| CI - Rust, CLI and VS Code | dev push, PR, manual, reusable call | Rust tests/format/Clippy, version checks, CLI smoke test, extension host test and Linux package inspection. |
| Release - CLI and Language Server | master push, manual recovery | Verifies the selected commit and calls each changed product independently. |
| Release - CLI | Coordinator only | Five CLI archives, SHA256SUMS, five platform npm packages and @luatlang/cli. |
| Release - Language Server and VS Code | Coordinator only | Five LSP archives and platform VSIX packages; publishes all five VSIX packages to the Visual Studio Marketplace, then publishes the GitHub release. |

CLI versions come from Cargo.toml and use tags such as `v0.1.6`. Language Server versions come from lsp/Cargo.toml and use `lsp-v0.1.0`. The bundled VS Code extension follows the Language Server version. LSP releases do not replace CLI as GitHub's latest release.

## Preparing a version

Run these from the repository root:

```powershell
npm run prepare:cli:patch
npm run prepare:lsp:patch
```

Use `major` or `minor` instead of `patch` when appropriate. Each command synchronizes only that product's package metadata and runs offline Cargo checks to update Cargo.lock. Existing `prepare:major/minor/patch` commands remain CLI aliases.

`npm run sync:versions:cli` and `npm run sync:versions:lsp` synchronize metadata without increasing versions. Release jobs check consistency and fail rather than silently changing committed versions.

A compiler fix reaches editor users only when an LSP release is also prepared, because the server embeds the library at its tagged source commit.

## Publishing

After review, commit the version and synchronized metadata and push/merge to master. Only changed product versions are eligible for release; introducing the LSP manifest is eligible for its initial release. Changing both versions creates two independent releases after shared verification. Documentation-only pushes do not publish.

All platform builds and packaging must succeed for a product before its draft release is created. Archives have SHA256 checksums. CLI npm publication completes before the draft becomes public. One product's packaging failure does not block the other product.

Creating a GitHub Release or pushing a tag alone does not start publication. Automatic and manual publishing both enter through release.yml. Stable numeric versions are required; prerelease tags are rejected.

## Recovery

Prefer **Re-run failed jobs** in the original run. To recover through a new run, open **Release - CLI and Language Server**, select master, choose `cli` or `lsp`, and enter the existing stable tag. The tag must be reachable from master and match its product manifest. Source checkout uses the tag's resolved commit, never the selected branch's latest code.

New-run recovery downloads the original prepared artifact referenced by the draft release. These artifacts are retained for 90 days; expired artifacts require explicit recovery of the original files. Rebuilding and replacing existing published bytes is deliberately rejected. Re-running all jobs in an existing run can hit immutable artifact-name conflicts; rerun only failed jobs or use a new manual recovery run.

Existing npm versions are skipped only if their integrity matches the original prepared tarball. Missing older versions are rejected rather than promoted to latest. Existing GitHub files must have matching hashes. Published releases with missing files require explicit restoration, and legacy tags with inconsistent package metadata fail validation.

npm, the Visual Studio Marketplace, and GitHub cannot publish atomically. If publication fails partway through, already published packages remain and the GitHub release stays a draft. Retries skip VS Code versions that are already published and complete matching CLI/GitHub assets without unpublishing anything.

## Repository setup

Configure npm Trusted Publishing for all six CLI packages and Visual Studio Marketplace Trusted Publishing for publisher `luat`, repository `laot7490/luat`, and this repository's top-level `release.yml` workflow. Leave the Marketplace environment blank unless the workflow is configured to use one. Publishing jobs request OIDC permission; no npm or VSCE token secret is needed. The workflows use Node 24, and CLI publishing pins npm 11.6.2.

If branch protection requires old check names, update it for **Verify Rust and CLI** and **Verify VS Code and Linux package** after migration. Account permissions/settings and live publishing are separate from local checks.
