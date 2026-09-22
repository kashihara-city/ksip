# Native AEC contract and synthetic echo test. No SIP and no application.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../dev-env.ps1"
Set-Location (Split-Path (Split-Path $PSScriptRoot))
cl /nologo /std:c++20 /EHsc /O2 /MT /DNOMINMAX /Isrc-native/webrtc /Itemp/build/native/include/ksip src-native/test-webrtc-audio.cpp /Fetemp/build/test-webrtc-audio.exe /Fotemp/build/test-webrtc-audio.obj /link /LIBPATH:temp/build/native/lib ksip_webrtc_audio.lib clang_rt.builtins-x86_64.lib winmm.lib crypt32.lib iphlpapi.lib secur32.lib oleaut32.lib advapi32.lib
if ($LASTEXITCODE -ne 0) { throw 'WebRTC audio ABI link test failed' }
& temp/build/test-webrtc-audio.exe
if ($LASTEXITCODE -ne 0) { throw 'WebRTC audio ABI smoke test failed' }
Write-Host 'Google WebRTC audio ABI smoke test passed'
if ($env:KSIP_TEST_AUDIO_DEVICE -eq '1') {
    & temp/build/test-webrtc-audio.exe --playout
    if ($LASTEXITCODE -ne 0) { throw 'WebRTC default playout callback test failed' }
    Write-Host 'Google WebRTC default playout callback test passed'
}
if ($env:KSIP_TEST_AUDIO_SIGNAL -eq '1') {
    $helper = 'temp/cargo-target/debug/examples/audio-devices.exe'
    if (!(Test-Path -LiteralPath $helper)) { throw 'Run scripts/test/rust.ps1 before the signal test' }
    $probe = Start-Process -FilePath 'temp/build/test-webrtc-audio.exe' -ArgumentList '--playout-signal' -WindowStyle Hidden -PassThru
    $maximum = 0.0
    try {
        while (!$probe.HasExited) {
            $sample = (& $helper peak speaker default | ConvertFrom-Json)
            if ($sample.peak -gt $maximum) { $maximum = [double]$sample.peak }
            Start-Sleep -Milliseconds 40
            $probe.Refresh()
        }
        if ($probe.ExitCode -ne 0) { throw "WebRTC signal playout failed ($($probe.ExitCode))" }
        if ($maximum -lt 0.01) { throw "WebRTC signal did not reach the Windows speaker endpoint (peak $maximum)" }
        Write-Host "Google WebRTC signal reached the Windows speaker endpoint (peak $maximum)"
    } finally {
        $probe.Refresh()
        if (!$probe.HasExited) { $probe.Kill() }
    }
}
