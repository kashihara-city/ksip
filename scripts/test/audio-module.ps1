# Build and run the unit tests of the audio module's device-free parts (the session core, the callback gate, the WAV recorder); needs MSVC, no device, no baresip.
. "$PSScriptRoot/../dev-env.ps1"
$root = Split-Path (Split-Path $PSScriptRoot)
Set-Location $root
New-Item -ItemType Directory -Force temp/build | Out-Null
cl /nologo /std:c++20 /EHsc /O2 /MT /DNOMINMAX /utf-8 /Isrc-native src-native/test-ksip-audio.cpp src-native/ksip_audio/recorder.cpp /Fetemp/build/test-ksip-audio.exe /Fotemp/build/ /link /SUBSYSTEM:CONSOLE
if ($LASTEXITCODE -ne 0) { throw 'test-ksip-audio did not build' }
& temp/build/test-ksip-audio.exe
if ($LASTEXITCODE -ne 0) { throw 'test-ksip-audio failed' }
