---
name: commit
description: Prepare a LuaT version, create a Conventional Commit, push it to a requested branch, and optionally publish a GitHub Release through the both flow.
---

# Commit and release LuaT changes

Use for `/commit <version> <branch> [release true|false]`. `<version>` is `major`, `minor`, `patch`, or `no`; `<branch>` is a branch name or `both`. The optional release argument defaults to `false`.

## Preflight

1. Read `AGENTS.md`; inspect the worktree, branch, reviewed diff, and recent local and remote history. Fetch relevant refs before comparing or merging. Stage only the requested changes; preserve unrelated edits and never amend an existing commit.
2. If `release` is `true`, require `<branch>` to be `both`; otherwise stop before changing files or Git history. Check `gh auth status --hostname github.com` and `gh repo view laot7490/luat` before any release-enabled mutation. If authentication or repository access is invalid, stop.
3. For release preflight, read the current Cargo version and calculate the resulting version: use it unchanged for `no`, or calculate the major/minor/patch bump. Check that `v<resulting-version>` does not exist as a local tag, on `origin`, or as a GitHub Release. If a check fails or its result is uncertain, stop before version preparation, commit, or push.

## Version and commit

1. For `major`, `minor`, or `patch`, run the corresponding repository script: `pnpm prepare:major`, `pnpm prepare:minor`, or `pnpm prepare:patch`. These update `Cargo.toml` and run `cargo check --offline`. For `no`, do not run a version script. Include version changes in the commit.
2. Review the final diff and choose a short Conventional Commit subject that describes the changes (`feat:`, `fix:`, `chore:`, etc.). If any requested check fails, stop before committing or pushing.
3. For a branch other than `both`, commit and push to that branch. If the release argument is `true`, the preflight rule means this path is not allowed.
4. For `both`, switch to `dev`, commit and push the requested changes, then switch to `master`, merge `dev` into `master`, and push `master`. Stop on a merge conflict without pushing `master`.

## GitHub Release

Run this section only when the release argument is `true`, after the `both` flow successfully pushed `master`.

1. Determine the previous published Release tag and review the full tree diff from that tag to the exact `master` commit being released, not only the latest commit message. This includes all staged and unstaged changes from the current request once they have been committed and pushed. Exclude unrelated or uncommitted changes that are not present in the Release commit. If there is no previous Release, summarize the repository history from its first commit. Use only changes supported by the diff and commit history.
2. Create `target/` if it does not exist, then write English release notes to `target/release-notes-v<version>.md` with exactly these two top-level sections:

   ```markdown
   ## Improvements
   - ...

   ## What's New
   - ...
   ```

   Keep the headings even when one category has no entries; do not invent changes to fill it.
3. Create the Release and version tag on `master` with GitHub CLI:

   ```powershell
   gh release create "v<version>" --repo laot7490/luat --title "LuaT v<version>" --target master --notes-file "target/release-notes-v<version>.md"
   ```

4. Confirm that GitHub CLI created the Release. Do not poll, watch, or wait for the Actions workflow or asset uploads; `release.published` starts them asynchronously. Report the commit hash, pushes, Release URL, and that asset building/upload is running without waiting. If Release creation fails after commits were pushed, report the partial outcome; do not roll back commits or silently recreate the Release.

## Stop conditions

- Do not send progress updates or narrate routine steps. Message the user only when a blocking error needs attention or after the requested commit, push, merge, and Release creation steps finish; never wait for Actions.
- On success, give a concise result with the current `Cargo.toml` version and commit/push status. When a Release was created, also include the release-notes file path, Release URL, and state that the asset workflow was triggered but not awaited. On a blocking error, report the error and any operations already completed.
- Do not create a missing requested branch silently.
- Stop if unrelated work cannot be safely separated or the requested changes are unclear.
- Stop if version preparation fails.
- Stop if the target Release tag already exists or GitHub authentication is invalid.
- Never report a commit, push, Release, or asset upload as successful without checking its result.
