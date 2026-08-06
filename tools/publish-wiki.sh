#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Publish doc/wiki/ to the GitHub wiki.
#
# The wiki is a separate git repo. Keeping the pages in this one means they get
# reviewed alongside the change that made them wrong, and a checkout carries its
# own documentation; this script is the one-way mirror.
#
# Usage: tools/publish-wiki.sh [--dry-run]

set -euo pipefail
cd "$(dirname "$0")/.."

SRC=doc/wiki
REMOTE=${WIKI_REMOTE:-git@github.com:cooljackal/flintos.wiki.git}
DRY_RUN=${1:-}

[ -d "$SRC" ] || { echo "no $SRC" >&2; exit 1; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

if ! git clone --quiet --depth 1 "$REMOTE" "$WORK/wiki" 2>/dev/null; then
    cat >&2 <<EOF
Could not clone $REMOTE

A GitHub wiki does not exist until it has at least one page. Create one:

  https://github.com/cooljackal/flintos/wiki  ->  "Create the first page"

Save anything; this script overwrites it.
EOF
    exit 1
fi

# Copy, then delete pages that no longer exist in source. Without the second
# step a renamed page lingers on the wiki forever, and stale docs are worse
# than missing ones because nothing signals they are stale.
for f in "$SRC"/*.md; do
    name=$(basename "$f")
    [ "$name" = "README.md" ] && continue   # explains the directory, not a page
    cp "$f" "$WORK/wiki/$name"
done

for f in "$WORK"/wiki/*.md; do
    name=$(basename "$f")
    if [ ! -f "$SRC/$name" ] || [ "$name" = "README.md" ]; then
        rm -f "$f"
        echo "removing stale page: $name"
    fi
done

cd "$WORK/wiki"

if git diff --quiet && git diff --cached --quiet && [ -z "$(git status --porcelain)" ]; then
    echo "wiki already up to date"
    exit 0
fi

git add -A
git status --short

if [ "$DRY_RUN" = "--dry-run" ]; then
    echo
    echo "dry run: nothing pushed"
    exit 0
fi

git -c user.name="$(git -C "$OLDPWD" config user.name)" \
    -c user.email="$(git -C "$OLDPWD" config user.email)" \
    commit --quiet -m "docs: sync from doc/wiki/"
git push --quiet
echo "published to $REMOTE"
