# Rust unit tests, plus the integration test that starts the real engine.
. "$PSScriptRoot/../dev-env.ps1"
$root = Split-Path (Split-Path $PSScriptRoot)
Set-Location $root
$env:KSIP_ENGINE_EXE = "$PSScriptRoot/../../temp/cargo-target/release/ksip.exe"
$generated = Mount-TauriGeneratedDirectory $root
try {
    cargo test --locked --manifest-path "$PSScriptRoot/../../src-tauri/Cargo.toml"
    if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }
    cargo test --locked --manifest-path "$PSScriptRoot/../../src-tauri/Cargo.toml" real_engine_starts_stops_and_restarts -- --ignored
    if ($LASTEXITCODE -ne 0) { throw 'Rust engine integration test failed' }
    cargo build --locked --manifest-path "$PSScriptRoot/../../src-tauri/Cargo.toml" --example audio-devices
    if ($LASTEXITCODE -ne 0) { throw 'Rust audio diagnostic build failed' }
} finally {
    Dismount-TauriGeneratedDirectory $generated
}
