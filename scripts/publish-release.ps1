# Publish a release snapshot from main to the public noviewlog repo.
# The public tree is `git archive main` (export-ignore strips internal
# materials) committed as a single squashed "Release vX.Y.Z" commit on the
# local public-main branch, then pushed to the public repo together with its
# tag. The GitHub release is created here with curated notes from
# release-notes/<tag>.md; CI (release.yml) only attaches the binaries.
#
# Draft notes mode: builds release-notes/<tag>.md from the commit titles of
# the release range (feat/perf -> Included, fix -> Fixes; merge/wip/test/
# chore/refactor/docs titles are left out on purpose). Review and edit the
# draft before publishing - the publish mode refuses files that still carry
# the DRAFT marker.
#
# Usage (Windows PowerShell, preferred):
#   .\scripts\publish-release.ps1 -DraftNotes -Tag v0.1.0 [-Base <sha>]
#   .\scripts\publish-release.ps1 -Tag v0.1.0
# Git Bash equivalent: scripts/publish-release.sh
param(
    [Parameter(Mandatory = $true)]
    [string]$Tag,
    [string]$NotesFile,
    [string]$Repo = 'nostalgie/noviewlog',
    [string]$Base,
    [switch]$DraftNotes
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

function Assert-LastExit {
    param([string]$Description)
    if ($LASTEXITCODE -ne 0) {
        Write-Error "$Description failed (exit $LASTEXITCODE)."
    }
}

if ($Tag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') {
    Write-Error "Tag '$Tag' must look like v0.1.0 (semver with a leading v)."
}

if (-not $NotesFile) {
    $NotesFile = Join-Path $Root "release-notes\$Tag.md"
}

# ---------- draft notes mode ----------
if ($DraftNotes) {
    if (-not $Base) {
        $stateFile = Join-Path $Root 'release-notes\.last-release-main.txt'
        if (Test-Path -LiteralPath $stateFile) {
            $Base = (Get-Content -LiteralPath $stateFile -Raw).Trim()
        }
    }
    if (-not $Base) {
        Write-Error 'No previous release recorded. For the first release pass -Base <commit-sha> (main history before it becomes the release range), or write the notes manually.'
    }
    if (Test-Path -LiteralPath $NotesFile) {
        Write-Error "Notes file already exists: $NotesFile - delete it or pass a different -NotesFile."
    }

    $subjects = git log --format=%s "$Base..main"
    Assert-LastExit "git log $Base..main"
    $total = $subjects.Count

    $parsed = foreach ($s in $subjects) {
        if ($s -match '^(Merge\b|Revert\b|wip\b|tmp\b)') { continue }
        if ($s -match '^(feat|feature|perf|fix)(\([^)]*\))?:\s*(.+)$') {
            $text = $Matches[3] -replace '\s*\((?:issue\s+)?#\d+\)\s*$', ''
            if ($text.Length -gt 0) {
                [pscustomobject]@{
                    IsFix = ($Matches[1] -eq 'fix')
                    Text  = $text.Substring(0, 1).ToUpper() + $text.Substring(1)
                }
            }
        }
    }

    $included = @()
    $fixes = @()
    foreach ($p in $parsed) {
        if ($p.IsFix) {
            if ($fixes -notcontains $p.Text) { $fixes += $p.Text }
        }
        else {
            if ($included -notcontains $p.Text) { $included += $p.Text }
        }
    }

    New-Item -ItemType Directory -Path (Split-Path -Parent $NotesFile) -Force | Out-Null
    $lines = @()
    $lines += '<!-- DRAFT: auto-generated from commit titles. Edit before publishing -'
    $lines += '     the publish mode refuses notes that still carry this DRAFT marker. -->'
    $lines += ''
    if ($included.Count -gt 0) {
        $lines += '## Included'
        $lines += ''
        $lines += ($included | ForEach-Object { "- $_" })
        $lines += ''
    }
    if ($fixes.Count -gt 0) {
        $lines += '## Fixes'
        $lines += ''
        $lines += ($fixes | ForEach-Object { "- $_" })
        $lines += ''
    }
    if ($included.Count -eq 0 -and $fixes.Count -eq 0) {
        $lines += '## Included'
        $lines += ''
        $lines += '- (no feat/perf/fix commits found in range - write the notes manually)'
        $lines += ''
    }
    $skipped = $total - $parsed.Count
    $lines += "<!-- range: $Base..main; $total commits total, $skipped left out (merge/wip/test/chore/refactor/docs and unprefixed titles). -->"

    Set-Content -LiteralPath $NotesFile -Value $lines -Encoding utf8
    Write-Host "Draft notes written: $NotesFile ($($included.Count) included, $($fixes.Count) fixes; $total commits in range)."
    Write-Host 'Review and edit the draft, then publish:'
    Write-Host "  .\scripts\publish-release.ps1 -Tag $Tag"
    exit 0
}

# ---------- publish mode ----------
$NotesPath = (Resolve-Path -LiteralPath $NotesFile -ErrorAction SilentlyContinue).Path
if (-not $NotesPath) {
    Write-Error "Release notes not found: $NotesFile (generate a draft first: -DraftNotes -Tag $Tag)."
}
if ((Get-Item -LiteralPath $NotesPath).Length -eq 0) {
    Write-Error "Release notes are empty: $NotesPath"
}
$notesRaw = Get-Content -LiteralPath $NotesPath -Raw
if ($notesRaw -match '<!--\s*DRAFT') {
    Write-Error "$NotesPath is still a generated draft - edit it first (remove the DRAFT marker when done)."
}

if ((git branch --show-current) -ne 'main') {
    Write-Error "Switch to main before publishing (current: $(git branch --show-current))."
}
if (git status --porcelain) {
    Write-Error "Working tree is not clean - commit or stash first."
}
git fetch origin main
Assert-LastExit 'git fetch origin main'
if ((git rev-parse main) -ne (git rev-parse origin/main)) {
    Write-Error "main is not in sync with origin/main - pull/push first."
}
if ((git remote) -notcontains 'public') {
    Write-Error "Remote 'public' is missing. Run: git remote add public git@github.com:$Repo.git"
}

$existing = git ls-remote --tags public "refs/tags/$Tag"
if ($existing) {
    Write-Error "Tag $Tag already exists in the public repo: $existing"
}

$TmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) "noviewlog-release-$Tag"
$Stage = Join-Path $TmpRoot 'stage'
$Wt = Join-Path $TmpRoot 'public-main'
$Tar = Join-Path $TmpRoot 'export.tar'

try {
    if (Test-Path -LiteralPath $TmpRoot) {
        Remove-Item -LiteralPath $TmpRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Path $TmpRoot | Out-Null

    Write-Host "==> Exporting main (export-ignore strips internal materials)..."
    git archive --format=tar --output="$Tar" main
    Assert-LastExit 'git archive main'
    New-Item -ItemType Directory -Path $Stage | Out-Null
    tar -xf "$Tar" -C "$Stage"
    Assert-LastExit 'tar -xf export.tar'

    foreach ($forbidden in @('openspec', '.cursor', '.kilo', 'release-notes', 'docs-private', 'AGENTS.md')) {
        if (Test-Path -LiteralPath (Join-Path $Stage $forbidden)) {
            Write-Error "Export sanity check failed: '$forbidden' is in the archive. Add it to .gitattributes export-ignore."
        }
    }

    # Content guard: exported files must never mention internal materials or
    # agent tooling - path absence alone does not prove content is clean.
    # Byte-level scan (Latin-1 maps bytes 1:1) so binary assets are covered.
    # Only the publish scripts themselves may contain these tokens; a hit in
    # any other file is fixed by scrubbing that file, never by widening the
    # allowlist (rule: docs-private/rules/public-docs-hygiene.md).
    $GuardTokens = @('docs-private', 'openspec', 'AGENTS.md', 'release-notes', '.cursor/', '.kilo')
    $GuardAllow = @('scripts/publish-release.ps1', 'scripts/publish-release.sh')
    $Latin1 = [System.Text.Encoding]::GetEncoding(28591)
    foreach ($file in (Get-ChildItem -LiteralPath $Stage -Recurse -File)) {
        $rel = $file.FullName.Substring($Stage.Length + 1).Replace('\', '/')
        if ($GuardAllow -contains $rel) { continue }
        $content = $Latin1.GetString([System.IO.File]::ReadAllBytes($file.FullName))
        $hits = @($GuardTokens | Where-Object { $content.Contains($_) })
        if ($hits.Count -gt 0) {
            Write-Error "Export content guard failed: '$rel' mentions internal material ($($hits -join ', ')). Scrub the source file; do not widen the allowlist."
        }
    }
    Write-Host "==> Content guard OK (no internal-material mentions outside the publish scripts)."

    git show-ref --verify --quiet refs/heads/public-main
    $hasBranch = ($LASTEXITCODE -eq 0)
    if ($hasBranch) {
        git worktree add $Wt public-main
        Assert-LastExit 'git worktree add'
    }
    else {
        Write-Host "==> First release: creating orphan branch public-main"
        git worktree add --detach $Wt HEAD
        Assert-LastExit 'git worktree add'
        git -C $Wt checkout --orphan public-main
        Assert-LastExit 'git checkout --orphan public-main'
    }

    git -C $Wt rm -r -f -q .
    Assert-LastExit 'git rm (clear worktree)'
    Copy-Item -Path (Join-Path $Stage '*') -Destination $Wt -Recurse -Force
    git -C $Wt add -A
    if ($hasBranch) {
        git -C $Wt diff --cached --quiet
        if ($LASTEXITCODE -eq 0) {
            Write-Error 'Public tree is identical to the previous release - nothing to publish.'
        }
    }
    git -C $Wt commit -q -m "Release $Tag"
    Assert-LastExit "git commit (Release $Tag)"
    $sha = git -C $Wt rev-parse HEAD

    Write-Host "==> Pushing public main and tag $Tag ($sha)..."
    git push public refs/heads/public-main:refs/heads/main
    Assert-LastExit 'git push public main'
    git tag $Tag $sha
    Assert-LastExit "git tag $Tag"
    git push public "refs/tags/$Tag"
    Assert-LastExit 'git push public tag'

    Write-Host "==> Creating GitHub release with curated notes..."
    gh release create $Tag --repo $Repo --verify-tag --latest --title $Tag --notes-file $NotesPath
    if ($LASTEXITCODE -ne 0) {
        Write-Host '  Release already exists - updating notes instead.'
        gh release edit $Tag --repo $Repo --latest --notes-file $NotesPath
        Assert-LastExit 'gh release edit'
    }

    git rev-parse main | Set-Content -LiteralPath (Join-Path $Root 'release-notes\.last-release-main.txt')

    Write-Host ''
    Write-Host "Release $Tag published to $Repo."
    Write-Host 'CI is building the binaries now: check Actions / release assets in a few minutes.'
}
finally {
    $ErrorActionPreference = 'Continue'
    if ($Wt -and (Test-Path -LiteralPath $Wt)) {
        git worktree remove --force $Wt 2>$null
    }
    git worktree prune 2>$null
    if (Test-Path -LiteralPath $TmpRoot) {
        Remove-Item -LiteralPath $TmpRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
