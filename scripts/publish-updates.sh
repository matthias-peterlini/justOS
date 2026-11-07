#!/bin/sh
# Publishes the signed index of updates (make updates) on the branch
# "updates" of the repository, where justOS looks for it
# (kernel/src/appupdate.rs). The branch holds only the latest index, its
# apps and the system image: each publication replaces it.
#
# Usage: scripts/publish-updates.sh [build/updates]
set -eu
DIR=${1:-build/updates}
[ -f "$DIR/index.json.sig" ] || { echo "no $DIR/index.json.sig: run make updates first" >&2; exit 1; }
REPO=$(pwd)
REMOTE=$(git remote get-url origin)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$DIR"/* "$WORK"/
cd "$WORK"
git init -q
git checkout -q -b updates
git add -A
SERIAL=$(sed -n 's/.*"serial":\([0-9]*\).*/\1/p' index.json)
git -c user.name="$(git -C "$REPO" config user.name)" -c user.email="$(git -C "$REPO" config user.email)" \
    commit -q -m "Updates, index $SERIAL"
git push -q -f "$REMOTE" updates
SYSTEM=$(ls justos-*.slot 2>/dev/null | sed 's/^justos-\(.*\)\.slot$/, justOS \1/')
echo "published index $SERIAL ($(ls *.app | wc -l) apps$SYSTEM)"
