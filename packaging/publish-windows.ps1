# Publishes the packaged release (zip + .minisig from `just package`) as a GitHub pre-release.
# The tag must already be pushed. See docs/releasing.md.
param(
    [Parameter(Mandatory = $true)][string]$Notes,
    [string]$Tag = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if (-not $Tag) { $Tag = "v$version-pre-alpha" }

$zip = Join-Path $root "dist\peeroxide-$version-windows-x64.zip"
$sig = "$zip.minisig"
foreach ($file in $zip, $sig, $Notes) {
    if (-not (Test-Path $file)) { throw "Missing $file (run 'just package' first; the updater needs both files)." }
}
if (Select-String -Path $Notes -Pattern 'Claude' -Quiet) {
    throw "The release notes mention Claude; remove that before publishing."
}

gh release create $Tag $zip $sig --verify-tag --prerelease --title "Peeroxide $version (pre-alpha)" --notes-file $Notes
if ($LASTEXITCODE -ne 0) { throw "gh release create failed." }
Write-Host "Published $Tag with $(Split-Path $zip -Leaf) and its signature."
