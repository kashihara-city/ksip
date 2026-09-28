# Clippy as the release workflow runs it, the Rust unit tests, and the integration test that starts the real engine.
. "$PSScriptRoot/../dev-env.ps1"
$root = Split-Path (Split-Path $PSScriptRoot)
Set-Location $root
$env:KSIP_ENGINE_EXE = "$PSScriptRoot/../../temp/cargo-target/release/ksip.exe"
$generated = Mount-TauriGeneratedDirectory $root
try {
    # The same lint run as the release workflow, so that a warning is caught here and not on the tag.
    cargo clippy --locked --all-targets --manifest-path "$PSScriptRoot/../../src-tauri/Cargo.toml" -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Clippy found something' }
    cargo test --locked --manifest-path "$PSScriptRoot/../../src-tauri/Cargo.toml"
    if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed' }
    # The tests that start the real engine (their names begin real_engine), ignored by a plain cargo test.
    cargo test --locked --manifest-path "$PSScriptRoot/../../src-tauri/Cargo.toml" real_engine -- --ignored --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw 'Rust engine integration test failed' }
    cargo build --locked --manifest-path "$PSScriptRoot/../../src-tauri/Cargo.toml" --example audio-devices
    if ($LASTEXITCODE -ne 0) { throw 'Rust audio diagnostic build failed' }
} finally {
    Dismount-TauriGeneratedDirectory $generated
}
