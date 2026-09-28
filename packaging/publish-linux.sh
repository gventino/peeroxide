#!/usr/bin/env bash
# Publishes the packaged Linux release (zip + .minisig from `just package`) to GitHub. The tag
# must already be pushed. If the release exists (e.g. published from Windows), the files are
# added to it; otherwise it is created as a pre-release. See docs/releasing.md.
set -euo pipefail

notes="${1:?usage: publish-linux.sh <notes.md> [tag]}"
root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)"
tag="${2:-v$version-pre-alpha}"

zip="$root/dist/peeroxide-$version-linux-x64.zip"
sig="$zip.minisig"
for file in "$zip" "$sig" "$notes"; do
    if [[ ! -f "$file" ]]; then
        echo "Missing $file (run 'just package' first; the updater needs both files)." >&2
        exit 1
    fi
done
if grep -q 'Claude' "$notes"; then
    echo "The release notes mention Claude; remove that before publishing." >&2
    exit 1
fi

if gh release view "$tag" > /dev/null 2>&1; then
    gh release upload "$tag" "$zip" "$sig"
    echo "Added $(basename "$zip") and its signature to $tag."
else
    gh release create "$tag" "$zip" "$sig" --verify-tag --prerelease \
        --title "Peeroxide $version (pre-alpha)" --notes-file "$notes"
    echo "Published $tag with $(basename "$zip") and its signature."
fi
