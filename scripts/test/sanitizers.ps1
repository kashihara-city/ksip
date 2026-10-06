# The native unit tests (test-ksip-audio, test-inband-dtmf, test-webrtc-audio)
# built with the clang-cl the engine is built with and its AddressSanitizer,
# then its UndefinedBehaviorSanitizer, and run: an overrun, a use after
# free or a double free in KSIP's own code stops the first, a signed
# overflow, a bad shift, a misaligned or null access the second. No device
# and no baresip. The three recorder tests that time themselves by the wall
# clock are left out (--no-clock): the instrumented code is slow enough to
# move what they measure.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../dev-env.ps1"
$root = (Split-Path (Split-Path $PSScriptRoot)).Replace('\','/')
Set-Location $root
$webrtc = if ($env:KSIP_WEBRTC_SOURCE) { $env:KSIP_WEBRTC_SOURCE } else { "$root/temp/w/src" }
$llvm = "$($webrtc.Replace('\','/'))/third_party/llvm-build/Release+Asserts"
$clang = "$llvm/bin/clang-cl.exe"
if (!(Test-Path -LiteralPath $clang)) { throw "The clang-cl the pinned WebRTC brings is missing: $clang" }
# The runtimes come with the compiler, under lib/clang/<version>/lib/windows;
# the ASan one is a DLL, which the test programs find beside themselves.
$runtime = Get-ChildItem -LiteralPath "$llvm/lib/clang" -Directory | Select-Object -First 1 | ForEach-Object { "$($_.FullName)/lib/windows" }
$out = "$root/temp/build/sanitizers"
New-Item -ItemType Directory -Force -Path $out | Out-Null
Copy-Item -LiteralPath "$runtime/clang_rt.asan_dynamic-x86_64.dll" -Destination $out -Force
# Reports with names: the symbolizer is beside the compiler.
$env:PATH = "$($llvm.Replace('/','\'))\bin;$env:PATH"
$env:ASAN_OPTIONS = 'halt_on_error=1'
$env:UBSAN_OPTIONS = 'halt_on_error=1:print_stacktrace=1'
$common = @('/nologo', '/std:c++20', '/EHsc', '/O1', '/Zi', '/MT', '/DNOMINMAX', '/D_MSVC_STL_HARDENING=1', '/utf-8', '/clang:-fno-omit-frame-pointer')
# What each program is made of, and what it is run with.
$programs = @(
    @{ name = 'test-ksip-audio'; sources = @('src-native/test/test-ksip-audio.cpp', 'src-native/ksip_audio/recorder.cpp', 'src-native/ksip/ksip_text.cpp')
       flags = @('/Isrc-native'); libs = @(); arguments = @('--no-clock') },
    @{ name = 'test-inband-dtmf'; sources = @('src-native/test/test-inband-dtmf.cpp', 'src-native/ksip_audio/inband_dtmf.cpp')
       flags = @('/DWIN32', '/Isrc-native', '/Itemp/build/native/include/re'); libs = @(); arguments = @() },
    # The WebRTC library is not instrumented, so the STL's container
    # annotations (which ASan would otherwise add on this side) are all off
    # here, or the linker refuses the mix; all of them, since each STL adds
    # to the list (vector and string, then optional).
    @{ name = 'test-webrtc-audio'; sources = @('src-native/test/test-webrtc-audio.cpp')
       flags = @('/Isrc-native/webrtc', '/Itemp/build/native/include/ksip', '/D_DISABLE_STL_ANNOTATION')
       libs = @('/LIBPATH:temp/build/native/lib', 'ksip_webrtc_audio.lib', 'clang_rt.builtins-x86_64.lib', 'winmm.lib', 'crypt32.lib', 'iphlpapi.lib', 'secur32.lib', 'oleaut32.lib', 'advapi32.lib')
       arguments = @() }
)
# UBSan with every check fatal, so that a finding is a failure, not a line
# in the output; ASan stops at its first finding by itself.
$sanitizers = @(
    @{ name = 'asan'; flags = @('-fsanitize=address') },
    @{ name = 'ubsan'; flags = @('-fsanitize=undefined', '-fno-sanitize-recover=all') }
)
foreach ($sanitizer in $sanitizers) {
    foreach ($program in $programs) {
        $exe = "$out/$($program.name)-$($sanitizer.name).exe"
        $objects = "$out/$($program.name)-$($sanitizer.name)"
        New-Item -ItemType Directory -Force -Path $objects | Out-Null
        # The UBSan runtime is built for the DLL CRT; with the static one the
        # linker notes each CRT import it makes (4217), and the result works.
        & $clang @common @($sanitizer.flags) @($program.flags) @($program.sources) "/Fe$exe" "/Fo$objects/" /link /SUBSYSTEM:CONSOLE /DEBUG /IGNORE:4217 @($program.libs)
        if ($LASTEXITCODE -ne 0) { throw "$($program.name) did not build with $($sanitizer.name)" }
        & $exe @($program.arguments)
        if ($LASTEXITCODE -ne 0) { throw "$($program.name) failed under $($sanitizer.name) ($LASTEXITCODE)" }
        Write-Host "$($program.name) passed under $($sanitizer.name)"
    }
}
