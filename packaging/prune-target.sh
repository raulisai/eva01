#!/bin/sh
# Deletes the stale build artifacts Cargo leaves behind. Every time a crate's
# fingerprint changes, Cargo writes a NEW `<name>-<hash>.<ext>` next to the old
# one and never removes the old — in this repo that grew target/ to 46 GB
# (37 copies of libobjc2_app_kit alone, ~191 MB each). For each (name, ext)
# only the most recently written file is kept; the rest can never be used
# again by the current source and are regenerated on demand if ever needed.
#
# Usage: packaging/prune-target.sh [profile ...]     (default: debug release)
# Safe to run any time; a running build is the only thing to avoid.

set -eu

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="$REPO_ROOT/target"
[ "$#" -gt 0 ] || set -- debug release

# prune_dir DIR HASH_RE: within DIR, group entries by "<name>-<hash>" + extension
# and delete all but the newest of each group.
prune_dir() {
    python3 - "$1" "$2" <<'PY'
import os, re, shutil, sys
d, h = sys.argv[1], sys.argv[2]
if not os.path.isdir(d):
    sys.exit()
pat = re.compile(r"^(.*)-" + h + r"(\..*)?$")
groups = {}
for e in os.scandir(d):
    m = pat.match(e.name)
    if m:
        groups.setdefault((m.group(1), m.group(2) or ""), []).append((e.stat(follow_symlinks=False).st_mtime, e.path))
for files in groups.values():
    files.sort(reverse=True)
    for _, path in files[1:]:
        shutil.rmtree(path) if os.path.isdir(path) and not os.path.islink(path) else os.remove(path)
PY
}

before="$(du -sk "$TARGET" 2>/dev/null | cut -f1)"
for profile in "$@"; do
    base="$TARGET/$profile"
    prune_dir "$base/deps" '[0-9a-f]{16}'
    # build/ is left alone on purpose: each package has TWO live dirs there (the
    # compiled build script and its run output, e.g. the Apple Intelligence
    # dylib) that share a name and differ only by hash, so "newest wins" would
    # delete the one build-app.sh needs.
    prune_dir "$base/incremental" '[0-9a-z]{13}' # base-36 hashes, not hex
done
after="$(du -sk "$TARGET" 2>/dev/null | cut -f1)"
echo "prune-target: $(( (before - after) / 1024 )) MB liberados ($(( after / 1024 )) MB restantes en target/)"
