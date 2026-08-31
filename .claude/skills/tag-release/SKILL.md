---
name: tag-release
description: Cut an iron_renamer release — bump the version, update the changelog, commit, and push the `v*` tag that builds every platform, creates the GitHub release, and submits to the Microsoft Store. Use when asked to release, cut a release, bump the version, or tag a version of iron_renamer.
---

# Releasing iron_renamer

Pushing a `v*` tag is the whole release. `.github/workflows/release.yml` builds
Windows x64/ARM64 (zip + MSIX), Linux x64/ARM64, and macOS, creates the GitHub
release with `--generate-notes --verify-tag`, and then submits the two MSIX
packages to Partner Center for certification. Nothing is published to
crates.io, and nothing ships from a push to master — only from a tag.

**A tag now reaches real users through the Microsoft Store**, which is a
certification queue you cannot quietly re-do. Build and test locally first; a
bad tag costs more than it used to.

`.claude/skills/tag-release/tag-release.ps1` is the last step. It reports by
default and only tags with `-Push`:

```powershell
pwsh -File .claude/skills/tag-release/tag-release.ps1          # check only
pwsh -File .claude/skills/tag-release/tag-release.ps1 -Push    # tag and push
```

It refuses unless the tree is clean, HEAD is `origin/master`, `Cargo.lock`
matches `Cargo.toml`, the changelog has a dated section for the version, and
the tag is free both locally and on origin.

## The release commit

Run the check first — it names whatever is missing.

1. Pick the number. Pre-1.0, so the minor carries user-visible change: new
   features or behaviour changes bump the minor (`0.4.2` → `0.5.0`), fixes and
   internal work bump the patch (`0.4.2` → `0.4.3`). A new tag, rule, or CLI
   flag is a minor.
2. `version` in `[package]` of `Cargo.toml`.
3. `cargo check` to write the new version through `Cargo.lock`. CI builds with
   `--locked`, so a stale lock fails the release on every platform at once.
   `packaging/windows/package.ps1` reads the same `Cargo.toml` line into the
   MSIX manifest, so the Store version follows from step 2 with nothing else to
   edit.
4. `CHANGELOG.md`: add `## <version> - <YYYY-MM-DD>` at the top, above the
   previous version. No `[Unreleased]` section and no link refs in this file —
   match the existing entries: a plain heading and a short bullet per
   user-visible change.
5. Commit as `Release <version>` (or fold it into the feature commit, as
   `d5a6628` did), and push master.

Then `-Push` the tag.

## After

`gh run watch` follows the build. The Store job runs last and needs the
`PARTNER_CENTER_*` secrets and the `PARTNER_CENTER_PRODUCT_ID` variable — see
the README. A failed run is fixed by fixing the commit and moving the tag —
delete it locally and on origin (`git push origin :refs/tags/v<version>`) and
run this again, rather than leaving a half-built release in place. If the Store
submission is the part that failed, check Partner Center before re-tagging: the
GitHub release already exists and `gh release create` will refuse the second
time.
