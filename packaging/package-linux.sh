#!/usr/bin/env bash
# Packages the release binary ($PEEROXIDE_EXE, default target/release/peeroxide) with the
# quickstart and third-party notices into dist/peeroxide-<version>[-<label>]-linux-x64/ and a
# .zip of that folder. `just package` builds it in a container first (`just release-portable`).
set -euo pipefail

label="${1:-}"
root="$(cd "$(dirname "$0")/.." && pwd)"
exe="$root/${PEEROXIDE_EXE:-target/release/peeroxide}"
if [[ ! -f "$exe" ]]; then
    echo "No release build at $exe; run 'just release-portable' first." >&2
    exit 1
fi

version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)"
if [[ -n "$label" ]]; then version="$version-$label"; fi
commit="$(git -C "$root" rev-parse --short HEAD)"
if [[ -n "$(git -C "$root" status --porcelain --untracked-files=no)" ]]; then
    echo "Warning: uncommitted changes are included in this build." >&2
    commit="$commit (with uncommitted changes)"
fi

name="peeroxide-$version-linux-x64"
dist="$root/dist"
dir="$dist/$name"
zip="$dir.zip"
# Only this package's own folder and zip are replaced; other releases in dist/ stay.
rm -rf "$dir" "$zip"
mkdir -p "$dir"

install -m 755 "$exe" "$dir/peeroxide"
title="Peeroxide $version - Linux (64-bit)"
underline="$(printf '%*s' "${#title}" '' | tr ' ' '=')"
sed -e "s|{title}|$title|" -e "s|{underline}|$underline|" -e "s|{commit}|$commit|" \
    "$root/packaging/QUICKSTART-linux.txt" > "$dir/QUICKSTART.txt"

# libde265's license (LGPL-3.0) must travel with the binary that contains it.
cat "$root/packaging/THIRD-PARTY-NOTICES.txt" "$root/crates/de265-sys/vendor/COPYING" \
    | tr -d '\r' > "$dir/THIRD-PARTY-NOTICES.txt"

(cd "$dist" && zip -qr "$zip" "$name")

# Sign the zip for the in-app updater (docs/releasing.md). Required for releases; optional for
# labelled test builds, which the updater never installs anyway.
sig="$zip.minisig"
rm -f "$sig"
key="${PEEROXIDE_RELEASE_KEY:-$HOME/.peeroxide/release.key}"
if [[ -f "$key" || -z "$label" ]]; then
    if ! (cd "$root" && cargo run -q --release -p peeroxide-update --example release-sign -- sign "$zip"); then
        echo "Signing failed; the release is not ready (see docs/releasing.md)." >&2
        exit 1
    fi
else
    echo "Warning: no signing key at $key, so this test build is unsigned." >&2
fi

hash="$(sha256sum "$dir/peeroxide" | cut -c1-16)"
size="$(du -m "$zip" | cut -f1)"
echo "Packaged $name (commit $commit)"
echo "  $zip (${size} MB)"
if [[ -f "$sig" ]]; then echo "  $sig"; fi
echo "  peeroxide SHA-256 starts with $hash (compare on each PC to be sure it's the same build)"
