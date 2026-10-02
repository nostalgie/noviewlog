#!/usr/bin/env bash
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
# Usage (Linux, or Git Bash on Windows):
#   scripts/publish-release.sh draft v0.1.0 [base-sha]
#   scripts/publish-release.sh v0.1.0 [notes-file] [repo]
set -euo pipefail

MODE="publish"
if [[ "${1:-}" == "draft" ]]; then
  MODE="draft"
  shift
fi

TAG="${1:?Usage: publish-release.sh [draft] v0.1.0 [base-sha | notes-file] [repo]}"
REPO="nostalgie/noviewlog"
NOTES_FILE=""

if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: tag '$TAG' must look like v0.1.0 (semver with a leading v)" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ -z "$NOTES_FILE" ]]; then
  NOTES_FILE="release-notes/${TAG}.md"
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

# ---------- publish mode ----------
NOTES_FILE="${2:-$NOTES_FILE}"
REPO="${3:-$REPO}"

if [[ ! -f "$NOTES_FILE" ]]; then
  echo "error: release notes not found: $NOTES_FILE (generate a draft first: $0 draft $TAG)" >&2
  exit 1
fi
if [[ ! -s "$NOTES_FILE" ]]; then
  echo "error: release notes are empty: $NOTES_FILE" >&2
  exit 1
fi
if grep -q '<!--[[:space:]]*DRAFT' "$NOTES_FILE"; then
  echo "error: $NOTES_FILE is still a generated draft - edit it first (remove the DRAFT marker when done)" >&2
  exit 1
fi

if [[ $(git branch --show-current) != "main" ]]; then
  echo "error: switch to main before publishing (current: $(git branch --show-current))" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree is not clean - commit or stash first" >&2
  exit 1
fi
git fetch --quiet origin main
if [[ "$(git rev-parse main)" != "$(git rev-parse origin/main)" ]]; then
  echo "error: main is not in sync with origin/main - pull/push first" >&2
  exit 1
fi
if ! git remote get-url public >/dev/null 2>&1; then
  echo "error: remote 'public' is missing. Run: git remote add public git@github.com:$REPO.git" >&2
  exit 1
fi
if git ls-remote --tags public "refs/tags/$TAG" | grep -q .; then
  echo "error: tag $TAG already exists in the public repo" >&2
  exit 1
fi

TMP="$(mktemp -d)"
STAGE="$TMP/stage"
WT="$TMP/public-main"
TAR="$TMP/export.tar"
trap 'git worktree remove --force "$WT" >/dev/null 2>&1 || true; git worktree prune >/dev/null 2>&1 || true; rm -rf "$TMP"' EXIT

mkdir -p "$STAGE"

echo "==> Exporting main (export-ignore strips internal materials)..."
git archive --format=tar --output="$TAR" main
tar -xf "$TAR" -C "$STAGE"

for forbidden in openspec .cursor .kilo release-notes docs-private AGENTS.md; do
  if [[ -e "$STAGE/$forbidden" ]]; then
    echo "error: export sanity check failed: '$forbidden' is in the archive. Add it to .gitattributes export-ignore." >&2
    exit 1
  fi
done

# Content guard: exported files must never mention internal materials or
# agent tooling - path absence alone does not prove content is clean. Only
# the publish scripts themselves may contain these tokens; a hit in any
# other file is fixed by scrubbing that file, never by widening the
# allowlist (rule: docs-private/rules/public-docs-hygiene.md).
GUARD_ALLOW=" scripts/publish-release.ps1 scripts/publish-release.sh "
GUARD_TOKENS=(-e 'docs-private' -e 'openspec' -e 'AGENTS.md' -e 'release-notes' -e '.cursor/' -e '.kilo')
while IFS= read -r -d '' f; do
  rel="${f#"$STAGE"/}"
  case "$GUARD_ALLOW" in *" $rel "*) continue ;; esac
  if LC_ALL=C grep -F -q "${GUARD_TOKENS[@]}" "$f"; then
    echo "error: export content guard failed: '$rel' mentions internal material:" >&2
    LC_ALL=C grep -F -o "${GUARD_TOKENS[@]}" "$f" | LC_ALL=C sort -u >&2
    echo "       scrub the source file; do not widen the allowlist" >&2
    exit 1
  fi
done < <(find "$STAGE" -type f -print0)
echo "==> Content guard OK (no internal-material mentions outside the publish scripts)."

FIRST=0
if git show-ref --verify --quiet refs/heads/public-main; then
  git worktree add -q "$WT" public-main
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
  echo "error: public tree is identical to the previous release - nothing to publish" >&2
  exit 1
fi
git -C "$WT" commit -q -m "Release $TAG"
SHA="$(git -C "$WT" rev-parse HEAD)"

echo "==> Pushing public main and tag $TAG ($SHA)..."
git push public refs/heads/public-main:refs/heads/main
git tag "$TAG" "$SHA"
git push public "refs/tags/$TAG"

echo "==> Creating GitHub release with curated notes..."
if ! gh release create "$TAG" --repo "$REPO" --verify-tag --latest --title "$TAG" --notes-file "$NOTES_FILE"; then
  gh release edit "$TAG" --repo "$REPO" --latest --notes-file "$NOTES_FILE"
fi

git rev-parse main > release-notes/.last-release-main.txt

echo
echo "Release $TAG published to $REPO."
echo "CI is building the binaries now: check Actions / release assets in a few minutes."
