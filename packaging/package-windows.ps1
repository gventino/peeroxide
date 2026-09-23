# Packages target\release\peeroxide.exe with the quickstart into
# dist\peeroxide-<version>[-<label>]-windows-x64\ and a .zip of that folder.
# Run `cargo build --release -p peeroxide` first (`just package` does both).
param([string]$Label = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\release\peeroxide.exe"
if (-not (Test-Path $exe)) {
    throw "No release build at $exe; run 'cargo build --release -p peeroxide' first."
}

$version = (Select-String -Path (Join-Path $root "Cargo.toml") -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if ($Label) { $version = "$version-$Label" }
$commit = (git -C $root rev-parse --short HEAD).Trim()
if (git -C $root status --porcelain --untracked-files=no) {
    Write-Warning "Uncommitted changes are included in this build."
    $commit = "$commit (with uncommitted changes)"
}

$name = "peeroxide-$version-windows-x64"
$dist = Join-Path $root "dist"
$dir = Join-Path $dist $name
$zip = "$dir.zip"
# Only this package's own folder and zip are replaced; other releases in dist\ stay.
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
if (Test-Path $zip) { Remove-Item -Force $zip }
New-Item -ItemType Directory -Force $dir | Out-Null

Copy-Item $exe $dir
$title = "Peeroxide $version - Windows (64-bit)"
$text = Get-Content (Join-Path $PSScriptRoot "QUICKSTART.txt") -Raw
$text = $text.Replace("{title}", $title).Replace("{underline}", "=" * $title.Length)
$text = $text.Replace("{commit}", $commit)
# CRLF so it reads well in any Windows text editor.
$text = $text -replace "`r?`n", "`r`n"
[System.IO.File]::WriteAllText((Join-Path $dir "QUICKSTART.txt"), $text, [System.Text.Encoding]::ASCII)

Compress-Archive -Path $dir -DestinationPath $zip

$hash = (Get-FileHash (Join-Path $dir "peeroxide.exe") -Algorithm SHA256).Hash.Substring(0, 16)
$size = "{0:N1} MB" -f ((Get-Item $zip).Length / 1MB)
Write-Host "Packaged $name (commit $commit)"
Write-Host "  $zip ($size)"
Write-Host "  peeroxide.exe SHA-256 starts with $hash (compare on each PC to be sure it's the same build)"
