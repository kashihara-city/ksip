# Build the Tauri application and place release/ksip.exe and ksip-v<version>.exe.
param([switch]$Clean)
. "$PSScriptRoot/../dev-env.ps1"
$root = Split-Path (Split-Path $PSScriptRoot)
Set-Location $root
$release = Join-Path $root 'release'
# A copy running from the release folder would lock the files this build replaces.
# On a development machine the build wins, so it is stopped before anything is built.
foreach ($running in @(Get-CimInstance Win32_Process -Filter "Name LIKE 'ksip%'" -ErrorAction SilentlyContinue |
        Where-Object { $_.ExecutablePath -like "$release\*" })) {
    Write-Host "Stopping $($running.Name) ($($running.ProcessId)) before building"
    Stop-Process -Id $running.ProcessId -Force -ErrorAction SilentlyContinue
}
python -X utf8 scripts/build/embed-notices.py
if ($LASTEXITCODE -ne 0) { throw 'License preparation failed' }
# Reproducible output: no timestamp, and no build machine paths in the binary.
# RUSTFLAGS replaces .cargo/config.toml's rustflags, so the static CRT is repeated here.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { "$env:USERPROFILE\.cargo" }
$env:RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=/Brepro " +
    "-C link-arg=/PDBALTPATH:%_PDB% " +
    "--remap-path-prefix=$root=. --remap-path-prefix=$cargoHome=.cargo"
$generated = Mount-TauriGeneratedDirectory $root
try {
    # A release build discards every earlier compilation, so nothing built under
    # other flags can be reused.
    if ($Clean) {
        cargo clean --release --manifest-path src-tauri/Cargo.toml
        if ($LASTEXITCODE -ne 0) { throw 'cargo clean failed' }
    }
    cargo build --release --locked --features custom-protocol --manifest-path src-tauri/Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw 'Tauri build failed' }
} finally {
    Dismount-TauriGeneratedDirectory $generated
}
New-Item -ItemType Directory -Force -Path $release | Out-Null
$manifest = Get-Content -LiteralPath "$root/src-tauri/Cargo.toml" -Raw
if ($manifest -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') { throw 'Cargo package version was not found' }
# Only this build's executables are replaced. Call history, recordings and the
# engine profile live in the same folder once the app has run, and must survive
# a build. Copies of earlier versions stay too: they are kept for comparison.
foreach ($name in 'ksip.exe', "ksip-v$($Matches[1]).exe") {
    $stale = Join-Path $release $name
    if (Test-Path -LiteralPath $stale) { Remove-Item -LiteralPath $stale -Force }
}
# The folder keeps a fixed name to run and a versioned copy of the same build.
Copy-Item -LiteralPath "$root/temp/cargo-target/release/ksip.exe" -Destination "$release/ksip.exe"
Copy-Item -LiteralPath "$release/ksip.exe" -Destination "$release/ksip-v$($Matches[1]).exe"
