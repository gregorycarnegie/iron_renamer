#Requires -Version 7
<#
.SYNOPSIS
    Check that this commit is a releasable one, then tag it and push the tag.
.DESCRIPTION
    A `v*` tag is the release: .github/workflows/release.yml builds every
    platform, creates the GitHub release, and submits the MSIX packages to the
    Microsoft Store. Everything here is a check that the tag will build
    something checkoutable; -Push actually creates it.
#>
[CmdletBinding()]
param(
    # Without this the script only reports; with it, it tags and pushes.
    [switch]$Push
)

$ErrorActionPreference = 'Stop'
Set-Location (git rev-parse --show-toplevel)

$problems = @()

# --locked doubles as the Cargo.lock check: CI builds with --locked, so a lock
# that still records the old version fails the release build on every platform.
$meta = cargo metadata --locked --format-version 1 --no-deps 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host 'not releasable:' -ForegroundColor Red
    Write-Host '  - Cargo.lock is out of sync with Cargo.toml — run `cargo check` and commit it'
    Write-Host ($meta | Out-String).TrimEnd()
    exit 1
}
$version = ($meta | ConvertFrom-Json).packages[0].version
if (-not $version) { throw 'could not read the version from cargo metadata' }
$tag = "v$version"

if (git status --porcelain) {
    $problems += "the working tree is not clean:`n" + (git status --short | Out-String).TrimEnd()
}

$branch = git rev-parse --abbrev-ref HEAD
if ($branch -ne 'master') { $problems += "on branch $branch, not master" }

# A tag on a commit origin has never seen builds a release nobody can check out,
# and `gh release create --verify-tag` fails outright.
git fetch --quiet origin master
if ((git rev-parse HEAD) -ne (git rev-parse origin/master)) {
    $problems += 'HEAD is not what origin/master points at — push the release commit first'
}

if (git tag --list $tag) { $problems += "tag $tag already exists locally" }
if (git ls-remote --tags origin "refs/tags/$tag") { $problems += "tag $tag already exists on origin" }

# The changelog is the human release notes; --generate-notes only lists commits.
$changelogPath = 'CHANGELOG.md'
if (-not (Test-Path $changelogPath)) {
    $problems += "$changelogPath is missing"
} elseif ((Get-Content $changelogPath -Raw) -notmatch
          "(?m)^## $([regex]::Escape($version)) - \d{4}-\d{2}-\d{2}\s*$") {
    $problems += "$changelogPath has no '## $version - <date>' section"
}

if ($problems) {
    Write-Host "not releasable as ${tag}:" -ForegroundColor Red
    $problems | ForEach-Object { Write-Host "  - $_" }
    exit 1
}

Write-Host "$tag is releasable: $(git rev-parse --short HEAD) on master, tag free"
if (-not $Push) {
    Write-Host 'dry run — re-run with -Push to tag and push it.'
    exit 0
}

git tag -a $tag -m "Release $tag"
git push origin $tag
Write-Host "pushed $tag — the release workflow builds and publishes it now (gh run watch)."
