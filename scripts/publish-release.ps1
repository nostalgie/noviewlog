# Publish a release snapshot from main to the public noviewlog repo.
# The public tree is `git archive main` (export-ignore strips internal
# materials) committed as a single squashed "Release vX.Y.Z" commit on the
# local public-main branch, then pushed to the public repo together with its
# tag. The GitHub release is created here with curated notes from
# release-notes/<tag>.md; CI (release.yml) only attaches the binaries.
#
# Modes:
#   -DraftNotes      - build release-notes/<tag>.md from commit titles
#                      (feat/perf -> Included, fix -> Fixes; merge/wip/test/
#                      chore/refactor/docs are left out on purpose). Review
#                      and edit the draft before publishing - publish refuses
#                      notes that still carry the DRAFT marker.
#   -DryRun          - run every publish precondition: export, path checks,
#                      content guards, CI gate, release commit - then STOP
#                      before any push and print the diff report against the
#                      current public tree.
#   -HotfixRef <ref> - re-issue an already published tag from its recorded
#                      base plus a reviewed patch branch (never from current
#                      main); the base comes from release-notes/<tag>.base,
#                      the CI gate runs on <ref>.
#   (default)        - the real thing: push public main + tag, create the
#                      GitHub release, record release-notes/<tag>.base.
#
# Every publish/hotfix writes release-notes/<tag>.base - the private SHA the
# exported tree came from. It is export-ignored (release-notes/ is) and is
# the only legal base for a later hotfix of that tag.
#
# Usage (Windows PowerShell, preferred):
#   .\scripts\publish-release.ps1 -DraftNotes -Tag v0.1.0 [-Base <sha>]
#   .\scripts\publish-release.ps1 -DryRun -Tag v0.1.0
#   .\scripts\publish-release.ps1 -HotfixRef <patch-ref> -Tag v0.1.0
#   .\scripts\publish-release.ps1 -Tag v0.1.0
# Git Bash equivalent: scripts/publish-release.sh
param(
    [Parameter(Mandatory = $true)]
    [string]$Tag,
    [string]$NotesFile,
    [string]$Repo = 'nostalgie/noviewlog',
    [string]$Base,
    [switch]$DraftNotes,
    [switch]$DryRun,
    [string]$HotfixRef
)

$ErrorActionPreference = 'Stop'
$KeepStage = $false

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

$Mode = 'publish'
if ($DraftNotes) { $Mode = 'draft' }
elseif ($HotfixRef) { $Mode = 'hotfix' }
elseif ($DryRun) { $Mode = 'dry-run' }

if (-not $NotesFile) {
    $NotesFile = Join-Path $Root "release-notes\$Tag.md"
}

# ---------- draft notes mode ----------
if ($Mode -eq 'draft') {
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
        # Case-insensitive prefix filter, matching publish-release.sh.
        if ($s -match '^(merge|revert|wip|tmp)\b') { continue }
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

# ---------- publish / dry-run / hotfix ----------

# Notes: strict in publish/hotfix; dry-run tolerates a not-yet-written file
# (the runbook drafts notes first, but iterating on CI must not require them).
$StrictNotes = $true
if (-not (Test-Path -LiteralPath $NotesFile)) {
    if ($Mode -eq 'dry-run') {
        Write-Host "==> dry-run: notes not written yet ($NotesFile) - notes checks skipped."
        $StrictNotes = $false
    }
    else {
        Write-Error "Release notes not found: $NotesFile (generate a draft first: -DraftNotes -Tag $Tag)."
    }
}
if ($StrictNotes) {
    if ((Get-Item -LiteralPath $NotesFile).Length -eq 0) {
        Write-Error "Release notes are empty: $NotesFile"
    }
    $notesRaw = Get-Content -LiteralPath $NotesFile -Raw
    if ($notesRaw -match '<!--\s*DRAFT') {
        Write-Error "$NotesFile is still a generated draft - edit it first (remove the DRAFT marker when done)."
    }
    # Quality gate: the body without HTML comments must have real content
    # (protects against publishing an effectively empty release page).
    $stripped = [regex]::Replace($notesRaw, '<!--.*?-->', '', [System.Text.RegularExpressions.RegexOptions]::Singleline)
    if (-not $stripped.Trim()) {
        Write-Error "Release notes have no readable content (comments only): $NotesFile - write at least a summary."
    }
}

if ((git branch --show-current) -ne 'main') {
    Write-Error "Switch to main before publishing (current: $(git branch --show-current))."
}
if (git status --porcelain) {
    Write-Error 'Working tree is not clean - commit or stash first.'
}

# A hotfix re-issues an already published tag: the tree is its recorded base
# plus a reviewed patch - current main is deliberately irrelevant.
$BaseSha = ''
$PatchSha = ''
if ($Mode -eq 'hotfix') {
    $BaseFile = Join-Path $Root "release-notes\$Tag.base"
    if (-not (Test-Path -LiteralPath $BaseFile)) {
        Write-Error "No recorded base for $Tag ($BaseFile missing) - hotfix re-issues only releases published by this script."
    }
    $BaseSha = (Get-Content -LiteralPath $BaseFile -Raw).Trim()
    git rev-parse --verify --quiet "$BaseSha^{commit}" 2>$null
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Recorded base $BaseSha ($BaseFile) is not a commit in this repo."
    }
    git rev-parse --verify --quiet "$HotfixRef^{commit}" 2>$null
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Patch ref not found: $HotfixRef"
    }
    git merge-base --is-ancestor $BaseSha "$HotfixRef^{commit}"
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Patch ref $HotfixRef must build on the recorded base $BaseSha."
    }
    $PatchSha = git rev-parse "$HotfixRef^{commit}"
}
else {
    git fetch origin main
    Assert-LastExit 'git fetch origin main'
    if ((git rev-parse main) -ne (git rev-parse origin/main)) {
        Write-Error 'main is not in sync with origin/main - pull/push first.'
    }
}

# Release-only CI gate: tests.yml runs manually (workflow_dispatch), never
# on merges. A release ships only from an SHA with a green tests run in the
# private repo - no bypass; to publish without CI you would have to edit
# this script, which is the point. Hotfixes gate the patch branch head.
function Require-GreenTests {
    param([string]$Sha, [string]$HintRef)
    $json = gh api "repos/nostalgie/noviewlog-private/commits/$Sha/check-runs" 2>$null | Out-String
    if (-not $json.Trim()) {
        Write-Error "Cannot read check runs for $Sha in noviewlog-private (gh auth? network?)."
    }
    $checks = ConvertFrom-Json -InputObject $json
    $total = @($checks.check_runs).Count
    $failed = @($checks.check_runs | Where-Object { $_.conclusion -ne 'success' })
    if ($total -eq 0) {
        Write-Error "No CI runs on $Sha - run the pre-release check first: gh workflow run tests.yml --repo nostalgie/noviewlog-private --ref $HintRef"
    }
    if ($failed.Count -gt 0) {
        $failed | ForEach-Object { Write-Host "  not green: $($_.name) ($(if ($_.conclusion) { $_.conclusion } else { 'running' }))" }
        Write-Error "Tests are not green on $Sha - fix and re-run before publishing"
    }
    Write-Host "==> Release CI gate OK (green tests run on $Sha)."
}
$ReleaseSha = git rev-parse main
if ($Mode -eq 'hotfix') {
    Require-GreenTests -Sha $PatchSha -HintRef $HotfixRef
}
else {
    Require-GreenTests -Sha $ReleaseSha -HintRef 'main'
}

if ((git remote) -notcontains 'public') {
    Write-Error "Remote 'public' is missing. Run: git remote add public git@github.com:$Repo.git"
}

$existing = git ls-remote --tags public "refs/tags/$Tag"
if ($existing) {
    Write-Error "Tag $Tag already exists in the public repo: $existing (for a re-issue delete the release and tag first)"
}

# The local public-main branch mirrors public main (see header). A stale
# mirror would make the dry-run diff and the release commit lie.
$publicHasMain = [bool](git ls-remote --heads public main)
if ($publicHasMain) {
    git show-ref --verify --quiet refs/heads/public-main
    if ($LASTEXITCODE -ne 0) {
        git fetch public '+refs/heads/main:refs/heads/public-main'
        Assert-LastExit 'git fetch public (initialize public-main)'
        Write-Host '==> Local public-main initialized from public/main.'
    }
    git fetch public main
    Assert-LastExit 'git fetch public main'
    if ((git rev-parse public/main) -ne (git rev-parse public-main)) {
        Write-Error "Local public-main is out of sync with public/main - resync: git fetch public '+refs/heads/main:refs/heads/public-main'"
    }
}
else {
    Write-Host '==> Public repo has no main branch yet - first release flow.'
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

    $ExportRev = 'main'
    if ($Mode -eq 'hotfix') { $ExportRev = $BaseSha }
    Write-Host "==> Exporting $ExportRev (export-ignore strips internal materials)..."
    git archive --format=tar --output="$Tar" $ExportRev
    Assert-LastExit "git archive $ExportRev"
    New-Item -ItemType Directory -Path $Stage | Out-Null
    tar -xf "$Tar" -C "$Stage"
    Assert-LastExit 'tar -xf export.tar'

    # Internal material names, assembled from fragments so this shipped
    # script itself stays free of the literal tokens it forbids.
    foreach ($forbidden in @('open' + 'spec', '.cur' + 'sor', '.k' + 'ilo', 'release' + '-notes', 'docs' + '-private', 'AGENTS' + '.md')) {
        if (Test-Path -LiteralPath (Join-Path $Stage $forbidden)) {
            Write-Error "Export sanity check failed: a forbidden internal path is in the archive. Add it to .gitattributes export-ignore."
        }
    }

    # Content guard round 1 (exact tokens): exported files must never mention
    # internal materials, agent tooling, or the publishing machine's personal
    # data - path absence alone does not prove content is clean. Byte-level
    # scan (Latin-1 maps bytes 1:1) so binary assets are covered. Only the
    # publish scripts themselves may contain these tokens; a hit in any other
    # file is fixed by scrubbing that file, never by widening the allowlist
    # (rule: the public docs hygiene policy). The guard scans the exported
    # tree only - export-ignored files never reach it. The names below are
    # assembled from fragments so this shipped script itself stays free of
    # the literal tokens it forbids.
    $GuardTokens = @('docs' + '-private', 'open' + 'spec', 'AGENTS' + '.md', 'release' + '-notes', '.cur' + 'sor/', '.k' + 'ilo')
    # Personal-data tokens: home path, username, and hostname of the machine
    # running the publish. Matched case-insensitively so a mixed-case fixture
    # hostname is caught by the lower-case machine value; bare-word tokens
    # need >= 4 chars to stay usable on generic usernames/hosts.
    $homeBase = Split-Path -Leaf $HOME
    $PersonalTokens = @("$HOME", "/home/$homeBase", "C:\Users\$homeBase")
    foreach ($tok in @($env:USERNAME, $env:COMPUTERNAME)) {
        if ($tok -and $tok -ne 'localhost' -and $tok.Length -ge 4) { $PersonalTokens += $tok }
    }
    $GuardAllow = @('scripts/publish-release.ps1', 'scripts/publish-release.sh')

    # Round 2 (patterns): emails, LAN/loopback IPs, private keys, credential
    # token shapes. Value exemptions are universal placeholders only, not a
    # file allowlist: git@github.com (clone URLs in docs), RFC 2606 example
    # domains (synthetic fixture identities), 127.0.0.1 / 127.0.1.1
    # (loopback). 10.x is deliberately not flagged - too generic in docs and
    # fixtures.
    $EmailRegex = '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
    $IpRegex = '\b((192\.168|172\.(1[6-9]|2[0-9]|3[01]))\.[0-9]{1,3}\.[0-9]{1,3}|127\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3})\b'
    $KeyRegex = '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----'
    $TokRegex = '\b(ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|xox[baprs]-[A-Za-z0-9-]{10,}|glpat-[A-Za-z0-9_-]{15,})\b'
    $ExemptRegex = '^(git@github\.com|nostalgie@users\.noreply\.github\.com|i@izs\.me|127\.0\.0\.1|127\.0\.1\.1)$|(^|[.@])example\.(com|net|org)$'

    $Latin1 = [System.Text.Encoding]::GetEncoding(28591)
    foreach ($file in (Get-ChildItem -LiteralPath $Stage -Recurse -File)) {
        $rel = $file.FullName.Substring($Stage.Length + 1).Replace('\', '/')
        if ($GuardAllow -contains $rel) { continue }
        $content = $Latin1.GetString([System.IO.File]::ReadAllBytes($file.FullName))
        $hits = @($GuardTokens | Where-Object { $content.Contains($_) })
        if ($hits.Count -gt 0) {
            Write-Error "Export content guard failed: '$rel' mentions internal material ($($hits -join ', ')). Scrub the source file; do not widen the allowlist."
        }
        $lower = $content.ToLower()
        $phits = @($PersonalTokens | Where-Object { $lower.Contains($_.ToLower()) })
        if ($phits.Count -gt 0) {
            Write-Error "Export content guard failed: '$rel' contains personal data ($($phits -join ', ')). Scrub the source file; do not widen the allowlist."
        }
        # Match publish-release.sh: GNU grep skips -o output on binary files (NUL
        # bytes). Regex over Latin-1 font/png blobs otherwise false-positive emails.
        if ($content.IndexOf([char]0) -lt 0) {
            $values = @()
            foreach ($rx in @($EmailRegex, $IpRegex, $KeyRegex, $TokRegex)) {
                $values += ([regex]::Matches($content, $rx) | ForEach-Object { $_.Value })
            }
            $bad = @($values | Sort-Object -Unique | Where-Object { $_.ToLower() -notmatch $ExemptRegex })
            if ($bad.Count -gt 0) {
                Write-Error "Export content guard failed (patterns): '$rel' contains: $($bad -join ', '). Scrub the source file; do not widen the allowlist."
            }
        }
    }
    Write-Host '==> Content guard OK (tokens, personal data, patterns).'

    # Optional deep secret scan (D3): gitleaks, when installed, gets a second
    # look at the exported tree. Absence of the binary is not an error.
    if (Get-Command gitleaks -ErrorAction SilentlyContinue) {
        Write-Host '==> Running gitleaks (optional deep scan)...'
        $leaksReport = Join-Path $TmpRoot 'gitleaks.json'
        gitleaks detect --source $Stage --no-git --report-format json --report-path $leaksReport --redact --quiet 2>$null
        if ($LASTEXITCODE -ne 0) {
            if (Test-Path -LiteralPath $leaksReport) {
                Get-Content -LiteralPath $leaksReport -Raw | ConvertFrom-Json |
                    ForEach-Object { Write-Host "  $($_.File): $($_.RuleID)" }
            }
            Write-Error 'gitleaks flagged the export - review before publishing.'
        }
        Write-Host '==> gitleaks OK.'
    }
    else {
        Write-Host '==> gitleaks not found - deep secret scan skipped (optional).'
    }

    git show-ref --verify --quiet refs/heads/public-main
    $hasBranch = ($LASTEXITCODE -eq 0)
    if ($hasBranch) {
        # Detached on purpose: the release commit must not advance local
        # public-main until an actual publish - a dry-run leaves it
        # untouched, and a re-run never stacks squash commits.
        git worktree add --detach $Wt public-main
        Assert-LastExit 'git worktree add'
    }
    else {
        Write-Host '==> First release: creating orphan branch public-main'
        git worktree add --detach $Wt HEAD
        Assert-LastExit 'git worktree add'
        git -C $Wt checkout --orphan public-main
        Assert-LastExit 'git checkout --orphan public-main'
    }

    git -C $Wt rm -r -f -q .
    Assert-LastExit 'git rm (clear worktree)'
    Copy-Item -Path (Join-Path $Stage '*') -Destination $Wt -Recurse -Force
    git -C $Wt add -A
    # Copy-Item on Windows drops the executable bit; restore tracked +x shell scripts.
    foreach ($line in (git -C $Root ls-files -s 'scripts/*.sh')) {
        if ($line -match '^100755\s+\S+\s+\S+\s+(.+)$') {
            $rel = $Matches[1] -replace '\\', '/'
            git -C $Wt update-index --chmod=+x -- $rel
            Assert-LastExit "git update-index --chmod=+x $rel"
        }
    }
    if ($hasBranch) {
        git -C $Wt diff --cached --quiet
        if ($LASTEXITCODE -eq 0) {
            Write-Error 'Public tree is identical to the previous release - nothing to publish.'
        }
    }

    # Hotfix hard check: the release tree must differ from the current public
    # tree by exactly the reviewed patch - nothing more, nothing less.
    if ($Mode -eq 'hotfix') {
        $staged = @(git -C $Wt diff --cached --name-only | Sort-Object)
        $patchFiles = @(git diff --name-only $BaseSha $PatchSha | Sort-Object)
        $diff = @(Compare-Object -ReferenceObject $staged -DifferenceObject $patchFiles)
        if ($diff.Count -gt 0) {
            $extra = @($diff | Where-Object { $_.SideIndicator -eq '<=' } | ForEach-Object { $_.InputObject })
            $missing = @($diff | Where-Object { $_.SideIndicator -eq '=>' } | ForEach-Object { $_.InputObject })
            Write-Error "Hotfix tree check failed - the staged tree must differ from the public tree by exactly the patch files. Extra in export: $($extra -join ', '). Missing in export: $($missing -join ', ')."
        }
    }

    git -C $Wt commit -q -m "Release $Tag"
    Assert-LastExit "git commit (Release $Tag)"
    $sha = git -C $Wt rev-parse HEAD

    if ($Mode -eq 'dry-run') {
        Write-Host ''
        Write-Host '==> DRY-RUN report - nothing was pushed:'
        Write-Host "  release commit: $sha"
        if ($hasBranch) {
            Write-Host '  changes vs current public tree:'
            git -C $Wt diff --name-status public-main HEAD | ForEach-Object { Write-Host "    $_" }
            $infra = @(git -C $Wt diff --name-only public-main HEAD | Where-Object { $_ -match '^(\.github|\.cargo|scripts|packaging)/' })
            if ($infra.Count -gt 0) {
                Write-Host '  !! infrastructure files changed (workflows, cargo config, scripts, packaging):'
                $infra | ForEach-Object { Write-Host "    $_" }
                Write-Host '     these run in the PUBLIC repo - scripts/verify-export.sh on the stage is mandatory before publishing'
            }
        }
        else {
            $count = @(git -C $Wt ls-files).Count
            Write-Host "  first release: $count files"
        }
        if (-not $StrictNotes) {
            Write-Host "  notes: still missing - $NotesFile must exist and be final before publishing"
        }
        $baseForPointer = $ReleaseSha
        if ($Mode -eq 'hotfix') { $baseForPointer = $PatchSha }
        Write-Host "  base pointer would be: release-notes\$Tag.base = $baseForPointer"
        # Keep the stage for verify-export / inspection; drop the release
        # worktree (a registered git worktree) and skip the temp cleanup.
        if ($Wt -and (Test-Path -LiteralPath $Wt)) {
            git worktree remove --force $Wt 2>$null
        }
        git worktree prune 2>$null
        $KeepStage = $true
        Write-Host "  stage kept at: $Stage - run scripts/verify-export.sh on it before publishing"
        Write-Host '==> dry-run OK. Re-run without -DryRun to publish.'
        exit 0
    }

    # Materialize the release commit on the local mirror branch, then push
    # it (the worktree was detached so dry-runs and re-runs never stack).
    git branch -f public-main $sha
    Assert-LastExit 'git branch -f public-main'

    Write-Host "==> Pushing public main and tag $Tag ($sha)..."
    git push public refs/heads/public-main:refs/heads/main
    Assert-LastExit 'git push public main'
    git tag $Tag $sha
    Assert-LastExit "git tag $Tag"
    git push public "refs/tags/$Tag"
    Assert-LastExit 'git push public tag'

    Write-Host '==> Creating GitHub release with curated notes...'
    gh release create $Tag --repo $Repo --verify-tag --latest --title $Tag --notes-file $NotesFile
    if ($LASTEXITCODE -ne 0) {
        Write-Host '  Release already exists - updating notes instead.'
        gh release edit $Tag --repo $Repo --latest --notes-file $NotesFile
        Assert-LastExit 'gh release edit'
    }

    # Post-mortem base pointer: the private SHA this exported tree came from -
    # the only legal base for a later hotfix of this tag.
    $pointerSha = $ReleaseSha
    if ($Mode -eq 'hotfix') {
        $pointerSha = $PatchSha
    }
    else {
        git rev-parse main | Set-Content -LiteralPath (Join-Path $Root 'release-notes\.last-release-main.txt')
    }
    Set-Content -LiteralPath (Join-Path $Root "release-notes\$Tag.base") -Value $pointerSha -Encoding ascii

    Write-Host ''
    Write-Host "Release $Tag published to $Repo."
    Write-Host 'CI is building the binaries now: check Actions / release assets in a few minutes.'
    Write-Host 'Then finish the release per the release checklist:'
    Write-Host '  - green public CI and assets attached,'
    Write-Host "  - re-read 'gh release view $Tag --repo $Repo',"
    Write-Host '  - scan the public tree, chore-PR if needed.'
}
finally {
    $ErrorActionPreference = 'Continue'
    if ($Wt -and (Test-Path -LiteralPath $Wt)) {
        git worktree remove --force $Wt 2>$null
    }
    git worktree prune 2>$null
    if (-not $KeepStage -and (Test-Path -LiteralPath $TmpRoot)) {
        Remove-Item -LiteralPath $TmpRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
