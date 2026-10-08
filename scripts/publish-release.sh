#!/usr/bin/env bash
# Publish a release snapshot from main to the public noviewlog repo.
# The public tree is `git archive main` (export-ignore strips internal
# materials) committed as a single squashed "Release vX.Y.Z" commit on the
# local public-main branch, then pushed to the public repo together with its
# tag. The GitHub release is created here with curated notes from
# release-notes/<tag>.md; CI (release.yml) only attaches the binaries.
#
# Modes:
#   draft    - build release-notes/<tag>.md from commit titles (feat/perf ->
#              Included, fix -> Fixes; merge/wip/test/chore/refactor/docs are
#              left out on purpose). Review and edit the draft before
#              publishing - publish refuses notes that still carry the DRAFT
#              marker.
#   dry-run  - run every publish precondition: export, path checks, content
#              guards, CI gate, release commit - then STOP before any push
#              and print the diff report against the current public tree.
#   hotfix   - re-issue an already published tag from its recorded base plus
#              a reviewed patch branch (never from current main):
#              `hotfix <tag> <patch-ref>`; the base comes from
#              release-notes/<tag>.base, the CI gate runs on <patch-ref>.
#   publish  - the real thing: push public main + tag, create the GitHub
#              release, record release-notes/<tag>.base.
#
# Usage (Linux, or Git Bash on Windows):
#   scripts/publish-release.sh draft v0.1.0 [base-sha]
#   scripts/publish-release.sh dry-run v0.1.0 [notes-file]
#   scripts/publish-release.sh hotfix v0.1.0 <patch-ref> [notes-file]
#   scripts/publish-release.sh v0.1.0 [notes-file] [repo]
#
# Every publish/hotfix writes release-notes/<tag>.base - the private SHA the
# exported tree came from. It is export-ignored (release-notes/ is) and is
# the only legal base for a later hotfix of that tag.
set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

MODE="publish"
HOTFIX_REF=""
case "${1:-}" in
draft) MODE="draft"; shift ;;
dry-run) MODE="dry-run"; shift ;;
hotfix) MODE="hotfix"; shift ;;
esac

TAG="${1:?Usage: publish-release.sh [draft|dry-run|hotfix] v0.1.0 [base-sha | notes-file | patch-ref] [repo]}"
REPO="nostalgie/noviewlog"
NOTES_FILE=""
BASE_SHA=""

if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  die "tag '$TAG' must look like v0.1.0 (semver with a leading v)"
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ -z "$NOTES_FILE" ]]; then
  NOTES_FILE="release-notes/${TAG}.md"
fi

if [[ "$MODE" == "hotfix" ]]; then
  HOTFIX_REF="${2:?Usage: publish-release.sh hotfix $TAG <patch-ref> [notes-file]}"
  NOTES_FILE="${3:-$NOTES_FILE}"
fi

# ---------- draft notes mode ----------
if [[ "$MODE" == "draft" ]]; then
  BASE="${2:-}"
  if [[ -z "$BASE" && -f release-notes/.last-release-main.txt ]]; then
    BASE="$(tr -d '[:space:]' < release-notes/.last-release-main.txt)"
  fi
  if [[ -z "$BASE" ]]; then
    echo "error: no previous release recorded. For the first release pass the base commit (draft v0.1.0 <sha>), or write the notes manually" >&2
    exit 1
  fi
  if [[ -e "$NOTES_FILE" ]]; then
    echo "error: notes file already exists: $NOTES_FILE - delete it or pass a different notes file" >&2
    exit 1
  fi

  subjects="$(git log --format=%s "$BASE..main")"
  INCLUDED="$(printf '%s\n' "$subjects" \
    | grep -Eiv '^(merge |revert |wip|tmp)' \
    | sed -nE 's/^(feat|feature|perf)(\([^)]*\))?:[[:space:]]*(.+)$/\3/p' \
    | sed -E 's/[[:space:]]*\((issue[[:space:]]+)?#[0-9]+\)[[:space:]]*$//' \
    | awk 'NF { print toupper(substr($0,1,1)) substr($0,2) }' \
    | awk '!seen[$0]++' \
    | sed 's/^/- /')"
  FIXES="$(printf '%s\n' "$subjects" \
    | grep -Eiv '^(merge |revert |wip|tmp)' \
    | sed -nE 's/^fix(\([^)]*\))?:[[:space:]]*(.+)$/\2/p' \
    | sed -E 's/[[:space:]]*\((issue[[:space:]]+)?#[0-9]+\)[[:space:]]*$//' \
    | awk 'NF { print toupper(substr($0,1,1)) substr($0,2) }' \
    | awk '!seen[$0]++' \
    | sed 's/^/- /')"

  mkdir -p "$(dirname "$NOTES_FILE")"
  {
    echo '<!-- DRAFT: auto-generated from commit titles. Edit before publishing -'
    echo '     the publish mode refuses notes that still carry this DRAFT marker. -->'
    echo
    if [[ -n "$INCLUDED" ]]; then printf '## Included\n\n%s\n\n' "$INCLUDED"; fi
    if [[ -n "$FIXES" ]]; then printf '## Fixes\n\n%s\n\n' "$FIXES"; fi
    if [[ -z "$INCLUDED" && -z "$FIXES" ]]; then
      printf '## Included\n\n- (no feat/perf/fix commits found in range - write the notes manually)\n\n'
    fi
    echo "<!-- range: $BASE..main; commits without a feat/perf/fix prefix were left out on purpose. -->"
  } > "$NOTES_FILE"

  echo "Draft notes written: $NOTES_FILE"
  echo "Review and edit the draft, then publish:"
  echo "  scripts/publish-release.sh $TAG"
  exit 0
fi

# ---------- publish / dry-run / hotfix ----------
if [[ "$MODE" != "hotfix" ]]; then
  # In hotfix mode both positional slots are already consumed: the patch ref
  # (mandatory) and the optional notes file.
  NOTES_FILE="${2:-$NOTES_FILE}"
  REPO="${3:-$REPO}"
fi

# Notes: strict in publish/hotfix; dry-run tolerates a not-yet-written file
# (the runbook drafts notes first, but iterating on CI must not require them).
STRICT_NOTES=1
if [[ "$MODE" == "dry-run" && ! -f "$NOTES_FILE" ]]; then
  echo "==> dry-run: notes not written yet ($NOTES_FILE) - notes checks skipped."
  STRICT_NOTES=0
fi
if [[ "$STRICT_NOTES" -eq 1 ]]; then
  [[ -f "$NOTES_FILE" ]] || die "release notes not found: $NOTES_FILE (generate a draft first: $0 draft $TAG)"
  [[ -s "$NOTES_FILE" ]] || die "release notes are empty: $NOTES_FILE"
  if grep -q '<!--[[:space:]]*DRAFT' "$NOTES_FILE"; then
    die "$NOTES_FILE is still a generated draft - edit it first (remove the DRAFT marker when done)"
  fi
  # Quality gate: the body without HTML comments must have real content
  # (protects against publishing an effectively empty release page).
  notes_stripped="$(awk '
    BEGIN { inc = 0 }
    {
      s = ""; rest = $0
      while (1) {
        if (inc == 0) {
          i = index(rest, "<!--")
          if (i == 0) { s = s rest; break }
          s = s substr(rest, 1, i - 1)
          rest = substr(rest, i + 4)
          inc = 1
        } else {
          j = index(rest, "-->")
          if (j == 0) { break }
          rest = substr(rest, j + 3)
          inc = 0
        }
      }
      print s
    }' "$NOTES_FILE")"
  [[ -n "$(printf '%s' "$notes_stripped" | tr -d '[:space:]')" ]] \
    || die "release notes have no readable content (comments only): $NOTES_FILE - write at least a summary"
fi

if [[ $(git branch --show-current) != "main" ]]; then
  die "switch to main before publishing (current: $(git branch --show-current))"
fi
if [[ -n "$(git status --porcelain)" ]]; then
  die "working tree is not clean - commit or stash first"
fi
if [[ "$MODE" == "hotfix" ]]; then
  # A hotfix re-issues an already published tag: the tree is its recorded
  # base plus a reviewed patch - current main is deliberately irrelevant.
  BASE_FILE="release-notes/${TAG}.base"
  [[ -f "$BASE_FILE" ]] || die "no recorded base for $TAG ($BASE_FILE missing) - hotfix re-issues only releases published by this script"
  BASE_SHA="$(tr -d '[:space:]' < "$BASE_FILE")"
  git rev-parse --verify --quiet "${BASE_SHA}^{commit}" >/dev/null \
    || die "recorded base $BASE_SHA ($BASE_FILE) is not a commit in this repo"
  [[ -n "$HOTFIX_REF" ]] || die "hotfix needs a patch ref"
  git rev-parse --verify --quiet "${HOTFIX_REF}^{commit}" >/dev/null \
    || die "patch ref not found: $HOTFIX_REF"
  git merge-base --is-ancestor "$BASE_SHA" "$HOTFIX_REF" \
    || die "patch ref $HOTFIX_REF must build on the recorded base $BASE_SHA"
  PATCH_SHA="$(git rev-parse "${HOTFIX_REF}^{commit}")"
else
  git fetch --quiet origin main
  if [[ "$(git rev-parse main)" != "$(git rev-parse origin/main)" ]]; then
    die "main is not in sync with origin/main - pull/push first"
  fi
fi

# Release-only CI gate: tests.yml runs manually (workflow_dispatch), never
# on merges. A release ships only from an SHA with a green tests run in the
# private repo - no bypass; to publish without CI you would have to edit
# this script, which is the point. Hotfixes gate the patch branch head.
require_green_tests() {
  local sha="$1" hint_ref="$2"
  local json total failed
  json="$(gh api "repos/nostalgie/noviewlog-private/commits/$sha/check-runs" 2>/dev/null || true)"
  [[ -n "$json" ]] || die "cannot read check runs for $sha in noviewlog-private (gh auth? network?)"
  total="$(printf '%s' "$json" | jq '.check_runs | length')"
  failed="$(printf '%s' "$json" | jq '[.check_runs[] | select(.conclusion != "success")] | length')"
  if [[ "$total" -eq 0 ]]; then
    echo "error: no CI runs on $sha - run the pre-release check first:" >&2
    echo "  gh workflow run tests.yml --repo nostalgie/noviewlog-private --ref $hint_ref" >&2
    exit 1
  fi
  if [[ "$failed" -gt 0 ]]; then
    printf '%s' "$json" | jq -r '.check_runs[] | select(.conclusion != "success") | "  not green: \(.name) (\(.conclusion // "running"))"' >&2
    echo "error: tests are not green on $sha - fix and re-run before publishing" >&2
    exit 1
  fi
  echo "==> Release CI gate OK (green tests run on $sha)."
}
if [[ "$MODE" == "hotfix" ]]; then
  require_green_tests "$PATCH_SHA" "$HOTFIX_REF"
else
  RELEASE_SHA="$(git rev-parse main)"
  require_green_tests "$RELEASE_SHA" main
fi

if ! git remote get-url public >/dev/null 2>&1; then
  die "remote 'public' is missing. Run: git remote add public git@github.com:$REPO.git"
fi
if git ls-remote --tags public "refs/tags/$TAG" | grep -q .; then
  die "tag $TAG already exists in the public repo (for a re-issue delete the release and tag first)"
fi

# The local public-main branch mirrors public main (see header). A stale
# mirror would make the dry-run diff and the release commit lie.
PUBLIC_HAS_MAIN=0
if git ls-remote --heads public main | grep -q .; then
  PUBLIC_HAS_MAIN=1
fi
if [[ "$PUBLIC_HAS_MAIN" -eq 1 ]]; then
  if ! git show-ref --verify --quiet refs/heads/public-main; then
    git fetch --quiet public '+refs/heads/main:refs/heads/public-main'
    echo "==> Local public-main initialized from public/main."
  fi
  git fetch --quiet public main
  if [[ "$(git rev-parse public/main)" != "$(git rev-parse public-main)" ]]; then
    die "local public-main is out of sync with public/main - resync: git fetch public '+refs/heads/main:refs/heads/public-main'"
  fi
else
  echo "==> Public repo has no main branch yet - first release flow."
fi

TMP="$(mktemp -d)"
STAGE="$TMP/stage"
WT="$TMP/public-main"
TAR="$TMP/export.tar"
trap 'git worktree remove --force "$WT" >/dev/null 2>&1 || true; git worktree prune >/dev/null 2>&1 || true; rm -rf "$TMP"' EXIT

mkdir -p "$STAGE"

EXPORT_REV="main"
[[ "$MODE" == "hotfix" ]] && EXPORT_REV="$BASE_SHA"
echo "==> Exporting $EXPORT_REV (export-ignore strips internal materials)..."
git archive --format=tar --output="$TAR" "$EXPORT_REV"
tar -xf "$TAR" -C "$STAGE"

for forbidden in "open""spec" ".cur""sor" ".k""ilo" "release""-notes" "docs""-private" "AGENTS"".md"; do
  if [[ -e "$STAGE/$forbidden" ]]; then
    die "export sanity check failed: a forbidden internal path is in the archive. Add it to .gitattributes export-ignore."
  fi
done

# Content guard round 1 (exact tokens): exported files must never mention
# internal materials, agent tooling, or the publishing machine's personal
# data - path absence alone does not prove content is clean. Only the
# publish scripts themselves may contain these tokens; a hit in any other
# file is fixed by scrubbing that file, never by widening the allowlist
# (rule: the public docs hygiene policy). The guard scans the exported
# tree only - export-ignored files never reach it. The names below are
# assembled from fragments so this shipped script itself stays free of
# the literal tokens it forbids.
GUARD_ALLOW=" scripts/publish-release.ps1 scripts/publish-release.sh "
GUARD_TOKENS=(-e 'docs'"-private" -e 'open'"spec" -e 'AGENTS'".md" -e 'release'"-notes" -e '.cur'"sor/" -e '.k'"ilo")
# Personal-data tokens: home path, username, and hostname of the machine
# running the publish. Matched case-insensitively so a mixed-case fixture
# hostname is caught by the lower-case machine value; bare-word tokens need
# >= 4 chars to stay usable on generic usernames/hosts.
GUARD_PERSONAL=(-e "$HOME" -e "/home/${HOME##*/}" -e "C:\\Users\\${HOME##*/}")
for tok in "$(id -un 2>/dev/null || true)" "$(uname -n 2>/dev/null || true)"; do
  if [[ -n "$tok" && "$tok" != "localhost" && ${#tok} -ge 4 ]]; then
    GUARD_PERSONAL+=(-e "$tok")
  fi
done

# Round 2 (patterns): emails, LAN/loopback IPs, private keys, credential
# token shapes. Value exemptions are universal placeholders only, not a
# file allowlist: git@github.com (clone URLs in docs), RFC 2606 example
# domains (synthetic fixture identities), 127.0.0.1 / 127.0.1.1 (loopback).
# 10.x is deliberately not flagged - too generic in docs and fixtures.
GUARD_RES=(
  -e '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
  -e '\b((192\.168|172\.(1[6-9]|2[0-9]|3[01]))\.[0-9]{1,3}\.[0-9]{1,3}|127\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3})\b'
  -e '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----'
  -e '\b(ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[0-9A-Z]{16}|xox[baprs]-[A-Za-z0-9-]{10,}|glpat-[A-Za-z0-9_-]{15,})\b'
)
GUARD_RES_EXEMPT='^(git@github\.com|nostalgie@users\.noreply\.github\.com|i@izs\.me|127\.0\.0\.1|127\.0\.1\.1)$|(^|[.@])example\.(com|net|org)$'

while IFS= read -r -d '' f; do
  rel="${f#"$STAGE"/}"
  case "$GUARD_ALLOW" in *" $rel "*) continue ;; esac
  if LC_ALL=C grep -F -q "${GUARD_TOKENS[@]}" "$f"; then
    echo "error: export content guard failed: '$rel' mentions internal material:" >&2
    LC_ALL=C grep -F -o "${GUARD_TOKENS[@]}" "$f" | LC_ALL=C sort -u >&2
    echo "       scrub the source file; do not widen the allowlist" >&2
    exit 1
  fi
  if LC_ALL=C grep -F -i -q "${GUARD_PERSONAL[@]}" "$f"; then
    echo "error: export content guard failed: '$rel' contains personal data:" >&2
    LC_ALL=C grep -F -i -o "${GUARD_PERSONAL[@]}" "$f" | LC_ALL=C sort -u >&2
    echo "       scrub the source file; do not widen the allowlist" >&2
    exit 1
  fi
  if hits="$(LC_ALL=C grep -E -o -h "${GUARD_RES[@]}" "$f" 2>/dev/null | LC_ALL=C sort -u | LC_ALL=C grep -Ev "$GUARD_RES_EXEMPT")" && [[ -n "$hits" ]]; then
    echo "error: export content guard failed (patterns): '$rel' contains:" >&2
    printf '%s\n' "$hits" | sed 's/^/       /' >&2
    echo "       scrub the source file; do not widen the allowlist" >&2
    exit 1
  fi
done < <(find "$STAGE" -type f -print0)
echo "==> Content guard OK (tokens, personal data, patterns)."

# Optional deep secret scan (D3): gitleaks, when installed, gets a second
# look at the exported tree. Absence of the binary is not an error.
if command -v gitleaks >/dev/null 2>&1; then
  echo "==> Running gitleaks (optional deep scan)..."
  gitleaks_rc=0
  gitleaks detect --source "$STAGE" --no-git --report-format json \
    --report-path "$TMP/gitleaks.json" --redact --quiet 2>"$TMP/gitleaks.err" || gitleaks_rc=$?
  if [[ "$gitleaks_rc" -ne 0 ]]; then
    if [[ -s "$TMP/gitleaks.json" ]]; then
      jq -r '.[] | "  \(.File): \(.RuleID) \(.Description // "")"' "$TMP/gitleaks.json" >&2 || true
    else
      tail -5 "$TMP/gitleaks.err" >&2 || true
    fi
    die "gitleaks flagged the export (exit $gitleaks_rc) - review before publishing"
  fi
  echo "==> gitleaks OK."
else
  echo "==> gitleaks not found - deep secret scan skipped (optional)."
fi

if git show-ref --verify --quiet refs/heads/public-main; then
  # Detached on purpose: the release commit must not advance local
  # public-main until an actual publish - a dry-run leaves it untouched,
  # and a re-run never stacks squash commits.
  git worktree add -q --detach "$WT" public-main
  FIRST=0
else
  echo "==> First release: creating orphan branch public-main"
  git worktree add --detach -q "$WT" HEAD
  git -C "$WT" checkout -q --orphan public-main
  FIRST=1
fi

git -C "$WT" rm -rqf .
cp -a "$STAGE"/. "$WT"/
git -C "$WT" add -A
if [[ "$FIRST" -eq 0 ]] && git -C "$WT" diff --cached --quiet; then
  die "public tree is identical to the previous release - nothing to publish"
fi

# Hotfix hard check: the release tree must differ from the current public
# tree by exactly the reviewed patch - nothing more, nothing less.
if [[ "$MODE" == "hotfix" ]]; then
  staged="$(git -C "$WT" diff --cached --name-only | LC_ALL=C sort)"
  patch_files="$(git diff --name-only "$BASE_SHA" "$PATCH_SHA" | LC_ALL=C sort)"
  if [[ "$staged" != "$patch_files" ]]; then
    echo "error: hotfix tree check failed - the staged tree must differ from the public tree by exactly the patch files." >&2
    echo "  extra in export: $(comm -13 <(printf '%s\n' "$patch_files") <(printf '%s\n' "$staged") | tr '\n' ' ')" >&2
    echo "  missing in export: $(comm -23 <(printf '%s\n' "$patch_files") <(printf '%s\n' "$staged") | tr '\n' ' ')" >&2
    exit 1
  fi
fi

git -C "$WT" commit -q -m "Release $TAG"
SHA="$(git -C "$WT" rev-parse HEAD)"

if [[ "$MODE" == "dry-run" ]]; then  echo
  echo "==> DRY-RUN report - nothing was pushed:"
  echo "  release commit: $SHA"
  if [[ "$FIRST" -eq 0 ]]; then
    echo "  changes vs current public tree:"
    git -C "$WT" diff --name-status public-main HEAD | sed 's/^/    /'
    infra="$(git -C "$WT" diff --name-only public-main HEAD | grep -E '^(\.github|\.cargo|scripts|packaging)/' || true)"
    if [[ -n "$infra" ]]; then
      echo "  !! infrastructure files changed (workflows, cargo config, scripts, packaging):"
      printf '%s\n' "$infra" | sed 's/^/    /'
      echo "     these run in the PUBLIC repo - scripts/verify-export.sh on the stage is mandatory before publishing"
    fi
  else
    echo "  first release: $(git -C "$WT" ls-files | wc -l) files"
  fi
  [[ "$STRICT_NOTES" -eq 1 ]] || echo "  notes: still missing - $NOTES_FILE must exist and be final before publishing"
  base_for_pointer="$RELEASE_SHA"
  [[ "$MODE" == "hotfix" ]] && base_for_pointer="$PATCH_SHA"
  echo "  base pointer would be: release-notes/$TAG.base = $base_for_pointer"
  # Keep the stage for verify-export / inspection; drop the release worktree
  # (a registered git worktree) and disable the cleanup trap for it.
  git worktree remove --force "$WT" >/dev/null 2>&1 || true
  git worktree prune >/dev/null 2>&1 || true
  trap - EXIT
  echo "  stage kept at: $STAGE - run scripts/verify-export.sh on it before publishing"
  echo "==> dry-run OK. Re-run without 'dry-run' to publish."
  exit 0
fi

# Materialize the release commit on the local mirror branch, then push it
# (the worktree was detached so dry-runs and re-runs never stack commits).
git branch -f public-main "$SHA"

echo "==> Pushing public main and tag $TAG ($SHA)..."
git push public refs/heads/public-main:refs/heads/main
git tag "$TAG" "$SHA"
git push public "refs/tags/$TAG"

echo "==> Creating GitHub release with curated notes..."
if ! gh release create "$TAG" --repo "$REPO" --verify-tag --latest --title "$TAG" --notes-file "$NOTES_FILE"; then
  gh release edit "$TAG" --repo "$REPO" --latest --notes-file "$NOTES_FILE"
fi

# Post-mortem base pointer: the private SHA this exported tree came from -
# the only legal base for a later hotfix of this tag.
if [[ "$MODE" == "hotfix" ]]; then
  printf '%s\n' "$PATCH_SHA" > "release-notes/${TAG}.base"
else
  git rev-parse main > release-notes/.last-release-main.txt
  printf '%s\n' "$RELEASE_SHA" > "release-notes/${TAG}.base"
fi

# ---------- local desktop entry (Linux, Wayland window icon) ----------
# Wayland has no window-icon protocol: the shell matches the surface app-id
# (slint::set_xdg_app_id, "noviewlog-slint") against StartupWMClass in a
# desktop entry. Refresh the local entry so the dock/taskbar shows the icon.
if [[ -d "$HOME/.local/share/applications" ]]; then
  BIN="$ROOT/target/release-dev/noviewlog-slint"
  ICON_DIR="$HOME/.local/share/noviewlog"
  ICON="$ICON_DIR/icon.png"
  if [[ -f "$BIN" && -f "$ROOT/packaging/linux/noviewlog-slint.desktop.template" ]]; then
    mkdir -p "$ICON_DIR"
    cp -f "$ROOT/crates/noviewlog-slint/ui/assets/icon.png" "$ICON"
    sed -e "s|@BIN_PATH@|$BIN|g" -e "s|@ICON_PATH@|$ICON|g" \
      "$ROOT/packaging/linux/noviewlog-slint.desktop.template" \
      > "$HOME/.local/share/applications/noviewlog-slint.desktop"
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
    echo "==> Local desktop entry installed: ~/.local/share/applications/noviewlog-slint.desktop"
  fi
fi

echo
echo "Release $TAG published to $REPO."
echo "CI is building the binaries now: check Actions / release assets in a few minutes."
echo "Then finish the release per the release checklist:"
echo "  - green public CI and assets attached,"
echo "  - re-read 'gh release view $TAG --repo $REPO',"
echo "  - scan the public tree, chore-PR if needed."
