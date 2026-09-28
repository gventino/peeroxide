# Releasing

How to publish a new version of Peeroxide. Installed copies update themselves from GitHub Releases every time they start (UC-09, FR-17), so **a release reaches every tester as soon as it is published**. Only publish what you have tested.

## One-time setup: the signing key

The app only installs updates signed with the project's release key (NFR-15, AC-12). Even someone who takes over the GitHub account can't push code to testers without that key.

1. Run `just release-keygen`. It asks for a password twice, then writes:
   - the **secret key** to `%USERPROFILE%\.peeroxide\release.key` (`~/.peeroxide/release.key` on Linux; or the path in `PEEROXIDE_RELEASE_KEY`);
   - the **public key** to `crates/update/release-key.pub`.
2. Commit `crates/update/release-key.pub`. Every build made from then on trusts this key. Until this file holds a key, builds don't update at all.
3. Back up the secret key file **and** its password somewhere safe, such as a password manager or an encrypted USB stick. Never commit it (`*.key` is in `.gitignore`), never upload it, never paste it anywhere.

The current release key has the ID `1FAB31191B660C70`.

**Packaging on another machine** (e.g. the Linux build): copy the secret key file there (`~/.peeroxide/release.key`, `chmod 600`) by a private route such as a USB stick. Never create a second key: installed copies only trust this one.

**If the key is lost:** installed copies only trust the old key, so they will refuse everything signed with a new one. Create a new key with `cargo run --release -p peeroxide-update --example release-sign -- keygen --force`, commit the new public key, release, and ask testers to download that release by hand once. After that, updates work again.

**If the key leaks:** do the same immediately, and tell testers not to trust updates until they have installed the new version by hand.

**Bootstrap:** the first version that contains the updater (0.5.0) has to be installed by hand by everyone. Later versions arrive by themselves.

## Checklist

1. Everything for the release is merged into `main`, docs included: README, roadmap, requirements, and the "NEW" section of `packaging/QUICKSTART.txt`, which goes inside the zip.
2. **Bump the version:** change `version` in the workspace `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, then commit `Release X.Y.Z` on `main` and push.
3. **`just check`** (formatting, clippy, all tests).
4. **`just package`**: builds the release exe, zips it with the quickstart and the third-party notices (libde265's license), and **signs the zip** (it asks for the key password). Result:
   - `dist/peeroxide-X.Y.Z-windows-x64.zip`
   - `dist/peeroxide-X.Y.Z-windows-x64.zip.minisig`

   It refuses to finish without the key, and it checks the signature against the public key the app is built with.
5. **Smoke test** the exe in `dist/`: a broadcaster and a viewer, with sound.
6. **Test the update locally** (recommended). `just serve-release dist/peeroxide-X.Y.Z-windows-x64.zip` pretends to be GitHub on this PC. Copy an older build into a folder and start it with `PEEROXIDE_UPDATE_URL=http://127.0.0.1:8765/releases`. It should update, restart and show "Updated to X.Y.Z".
7. **Tag** the release commit and push the tag:
   `git tag -a vX.Y.Z-pre-alpha -m "Peeroxide X.Y.Z (pre-alpha)"`, then `git push origin vX.Y.Z-pre-alpha`
8. **Write the release notes** (template below) and add the checksums (`Get-FileHash <file> -Algorithm SHA256`).
9. **`just publish notes.md`**: creates the GitHub pre-release with the zip **and** its signature. The updater needs both.
10. **Check:** open the release page, then start an older installed version; it should update itself.

## Linux

Each release also gets a Linux package, built on a Linux machine from the same commit. The build runs in a Debian bookworm container (`packaging/Dockerfile.linux`, needs Docker), so the binary runs on any distribution with glibc 2.35 or newer and doesn't depend on the host's PipeWire headers:

1. **`just package`** on Linux (builds with `just release-portable`) → `dist/peeroxide-X.Y.Z-linux-x64.zip` and its `.minisig` (signed the same way).
2. Smoke test it, and optionally test the update with `just serve-release dist/peeroxide-X.Y.Z-linux-x64.zip`.
3. **`just publish notes.md`** on Linux: if the release already exists (published from Windows), it only **adds** the zip and signature to it; add a Linux line to the release notes and the checksums (`sha256sum <file>`) by hand on the release page. If it doesn't exist yet, it creates it.

Linux copies update themselves only from releases that have a `linux-x64` package, so publish one with every release. Linux builds before the 0.6.0 Linux package don't update.

## What the updater looks for

- Releases on `gventino/peeroxide`, including pre-releases. Drafts are ignored.
- The tag's `X.Y.Z` must be higher than the running version (labels such as `-pre-alpha` are ignored when comparing).
- Two assets named exactly `peeroxide-X.Y.Z-<platform>.zip` and `peeroxide-X.Y.Z-<platform>.zip.minisig`, with the same `X.Y.Z` as the tag, where `<platform>` is `windows-x64` or `linux-x64`. `just package` and `just publish` produce and upload exactly these.
- The signature's trusted comment must be `file:peeroxide-X.Y.Z-<platform>.zip`. `release-sign` writes it that way.

Consequences:
- **Never replace the files of a published version.** Fix it by releasing a new version.
- **Never publish test builds on GitHub:** testers would install them. Use `just serve-release` to test.
- Builds with a label (`just package test`) are named differently, so the updater never picks them up.

## Release notes template

```markdown
**Pre-alpha.** An early build for testing with friends. Expect rough edges and breaking changes between versions.

<one-line description of Peeroxide>

## Download

**Windows 10 (version 2004 or newer) or Windows 11, 64-bit:** `peeroxide-X.Y.Z-windows-x64.zip`. Unzip it and run `peeroxide.exe`; no installation is needed. Installed copies update themselves.

**Linux, 64-bit (X11 or Wayland, with PipeWire):** `peeroxide-X.Y.Z-linux-x64.zip`. Unzip it and run `./peeroxide`. Installed copies update themselves. Linux is barely tested.

To skip the "Windows protected your PC" warning: before unzipping, right-click the zip → Properties → tick **Unblock** → OK. Otherwise click More info → Run anyway. The app isn't code-signed yet; updates installed by the app never show the warning.

## New in X.Y.Z

- ...

## Known limitations

- ...

## Checksums (SHA-256)

- `peeroxide-X.Y.Z-windows-x64.zip`: `...`
- `peeroxide.exe` inside it: `...`
```

Don't credit tools or assistants in the notes. `just publish` refuses notes that mention Claude.
