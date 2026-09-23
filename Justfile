# Peeroxide development commands. Run `just` to list them.
# Install just with `cargo install just` (or `winget install Casey.Just`, or your package manager).

set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# List the recipes
default:
    @just --list --unsorted

# Build the app (debug)
build:
    cargo build -p peeroxide

# Run the app, e.g. `just run --profile a --broadcast test --share-audio`
run *args:
    cargo run -p peeroxide -- {{ args }}

# Two instances on this machine: Alice broadcasts the test pattern with sound, Bob watches
[windows]
demo: build
    Start-Process target/debug/peeroxide.exe -ArgumentList '--profile','demo-alice','--name','Alice','--broadcast','test','--share-audio'
    Start-Sleep -Seconds 2
    Start-Process target/debug/peeroxide.exe -ArgumentList '--profile','demo-bob','--name','Bob','--watch','alice'

# Two instances on this machine: Alice broadcasts the test pattern with sound, Bob watches
[unix]
demo: build
    target/debug/peeroxide --profile demo-alice --name Alice --broadcast test --share-audio &
    sleep 2
    target/debug/peeroxide --profile demo-bob --name Bob --watch alice &

# Format all code
fmt:
    cargo fmt --all

# Clippy with warnings as errors, as in CI
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run the tests; extra arguments go to cargo, e.g. `just test -p peeroxide-net`
test *args:
    cargo test --workspace {{ args }}

# Everything CI checks: formatting, clippy and tests
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Optimized build: target/release/peeroxide(.exe)
release:
    cargo build --release -p peeroxide

# Release build zipped with the quickstart and signed into dist/, e.g. `just package` or `just package test-label`
[windows]
package label="": release
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging/package-windows.ps1 {{ if label == "" { "" } else { "-Label " + label } }}

# One-time: create the release signing key (asks for a password) and build its public key into the app
release-keygen:
    cargo run -q --release -p peeroxide-update --example release-sign -- keygen

# Publish the packaged release to GitHub (tag already pushed): `just publish notes.md [tag]`
[windows]
publish notes tag="":
    powershell.exe -NoProfile -ExecutionPolicy Bypass -File packaging/publish-windows.ps1 -Notes '{{ notes }}' {{ if tag == "" { "" } else { "-Tag " + tag } }}

# Serve a packaged release on this PC to test the self-update: `just serve-release dist/peeroxide-X.Y.Z-windows-x64.zip`
serve-release zip port="8765":
    cargo run -q --release -p peeroxide-update --example serve-release -- {{ zip }} {{ port }}

# Record an audio source to audio-probe.wav: `just probe-audio`, `just probe-audio tone 5`, `just probe-audio <PID> --play`
probe-audio *args:
    cargo run --release -p peeroxide-audio --example audio-probe -- {{ args }}

# List screen capture sources and measure one's frame rate: `just probe-capture [index|title] [seconds]`
probe-capture *args:
    cargo run --release -p peeroxide-capture --example probe -- {{ args }}

# H.264 encoder benchmark: `just bench [source|test|scroll] [seconds] [720|1080|internet]`
bench *args:
    cargo run --release -p peeroxide-codec --example bench -- {{ args }}
