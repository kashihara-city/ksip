# Build Google WebRTC's audio library with the KSIP bridge using GN/Ninja.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../dev-env.ps1"
$root = Split-Path (Split-Path $PSScriptRoot)
$source = if ($env:KSIP_WEBRTC_SOURCE) { $env:KSIP_WEBRTC_SOURCE } else { "$root/temp/w/src" }
$depot = if ($env:KSIP_DEPOT_TOOLS) { $env:KSIP_DEPOT_TOOLS } else { "$root/temp/d" }
if (!(Test-Path -LiteralPath "$source/BUILD.gn")) { throw 'Run python scripts/deps/fetch-webrtc.py first' }
if (!(Test-Path -LiteralPath "$depot/gn.bat")) { throw 'Pinned depot_tools was not found' }
$env:PATH = "$depot;$env:PATH"
$env:DEPOT_TOOLS_UPDATE = '0'
$env:DEPOT_TOOLS_WIN_TOOLCHAIN = '0'
$env:vs2022_install = $vs
python -X utf8 "$root/scripts/build/patch-webrtc.py" $source
if ($LASTEXITCODE -ne 0) { throw 'WebRTC bridge preparation failed' }
$out = "$source/out/ksip"
$gnArgs = 'target_os=\"win\" target_cpu=\"x64\" is_debug=false is_component_build=false rtc_include_tests=false rtc_build_examples=false rtc_build_tools=false rtc_enable_protobuf=false rtc_use_h264=false symbol_level=0 use_custom_libcxx=false'
Push-Location $source
try {
    & "$depot/gn.bat" gen $out "--args=$gnArgs"
    if ($LASTEXITCODE -ne 0) { throw 'WebRTC GN generation failed' }
    & "$depot/autoninja.bat" -C $out ksip_webrtc_audio
    if ($LASTEXITCODE -ne 0) { throw 'WebRTC audio build failed' }
    $noticeDir = "$root/temp/build/webrtc-notices"
    New-Item -ItemType Directory -Force -Path $noticeDir | Out-Null
    python tools_webrtc/libs/generate_licenses.py --target //:ksip_webrtc_audio $noticeDir out/ksip
    if ($LASTEXITCODE -ne 0) { throw 'WebRTC license generation failed' }
} finally {
    Pop-Location
}
$prefix = "$root/temp/build/native"
New-Item -ItemType Directory -Force -Path "$prefix/lib","$prefix/include/ksip" | Out-Null
Copy-Item -LiteralPath "$out/obj/ksip_webrtc_audio.lib" -Destination "$prefix/lib/ksip_webrtc_audio.lib" -Force
$builtins = Get-ChildItem -LiteralPath "$source/third_party/llvm-build/Release+Asserts/lib/clang" -Recurse -Filter 'clang_rt.builtins-x86_64.lib' | Select-Object -First 1
if (!$builtins) { throw 'Clang builtins library was not generated' }
Copy-Item -LiteralPath $builtins.FullName -Destination "$prefix/lib/clang_rt.builtins-x86_64.lib" -Force
Copy-Item -LiteralPath "$root/src-native/webrtc/ksip_audio_bridge.h" -Destination "$prefix/include/ksip/ksip_audio_bridge.h" -Force
