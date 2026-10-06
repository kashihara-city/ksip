# Build WebRTC, re, baresip and the KSIP modules into temp/build/native.
param([switch]$Clean)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../dev-env.ps1"
$root = (Split-Path (Split-Path $PSScriptRoot)).Replace('\','/')
Set-Location $root
function Run([string]$exe, [string[]]$arguments) {
    & $exe @arguments
    if ($LASTEXITCODE -ne 0) { throw "$exe failed ($LASTEXITCODE)" }
}
$prefix = "$root/temp/build/native"
# CMake keeps its probe results in the build directories, and a stale one once
# changed a LibreSSL check in libre. A release build starts from empty ones.
if ($Clean) {
    foreach ($dir in 'libressl', 'opus', 'libg722', 're', 'ksip', 'baresip', 'native') {
        Remove-Item -LiteralPath "$root/temp/build/$dir" -Recurse -Force -ErrorAction SilentlyContinue
    }
}
Run powershell @('-NoProfile','-ExecutionPolicy','Bypass','-File','scripts/build/webrtc.ps1')
Run python @('scripts/build/patch-baresip.py')
# Reproducible output: /Brepro replaces timestamps with content hashes, and
# /d1trimfile drops the repository path from __FILE__, which re, baresip and
# LibreSSL keep in their messages. The same source then gives the same object
# wherever the repository sits. CMake splits these flags on spaces, so the
# repository path must not contain one. The prefix ends in a slash, not a
# backslash: quoted on a command line, a trailing backslash escapes the quote.
if ($root -match ' ') { throw "The repository path must not contain spaces: $root" }
# C4068 is MSVC's own <system_error> complaining about its own pragma, six
# hundred times per build; without it the real warnings can be read.
$flags = "/O2 /DNDEBUG /Brepro /wd4068 /d1trimfile:$root/"
$common = @('-G','Ninja','-DCMAKE_BUILD_TYPE=Release',"-DCMAKE_C_FLAGS_RELEASE=$flags","-DCMAKE_CXX_FLAGS_RELEASE=$flags",'-DCMAKE_EXE_LINKER_FLAGS=/Brepro bcrypt.lib','-DCMAKE_STATIC_LINKER_FLAGS=/Brepro',"-DCMAKE_INSTALL_PREFIX=$prefix",'-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded')
# TLS and the crypto SRTP needs. LibreSSL builds with the same CMake and Ninja
# as the rest, and libre supports it upstream.
Run cmake (@('-S','temp/vendor/libressl','-B','temp/build/libressl') + $common + @('-DBUILD_SHARED_LIBS=OFF','-DLIBRESSL_APPS=OFF','-DLIBRESSL_TESTS=OFF'))
Run cmake @('--build','temp/build/libressl','--parallel','8')
Run cmake @('--install','temp/build/libressl')
# Windows headers define X509_NAME as a macro, which breaks LibreSSL's own
# headers, so wincrypt.h is kept out of the sources that include them.
$tls = @("-DOPENSSL_ROOT_DIR=$prefix", "-DOPENSSL_INCLUDE_DIR=$prefix/include", "-DOPENSSL_SSL_LIBRARY=$prefix/lib/ssl.lib", "-DOPENSSL_CRYPTO_LIBRARY=$prefix/lib/crypto.lib", '-DUSE_OPENSSL=ON')
$tlsFlags = @("-DCMAKE_C_FLAGS_RELEASE=$flags /DNOCRYPT","-DCMAKE_CXX_FLAGS_RELEASE=$flags /DNOCRYPT")
# Wideband codecs. Both are static only: the app ships as one executable.
Run cmake (@('-S','temp/vendor/opus','-B','temp/build/opus') + $common + @('-DOPUS_BUILD_SHARED_LIBRARY=OFF','-DOPUS_BUILD_PROGRAMS=OFF','-DOPUS_BUILD_TESTING=OFF','-DOPUS_STATIC_RUNTIME=ON'))
Run cmake @('--build','temp/build/opus','--parallel','8')
Run cmake @('--install','temp/build/opus')
Run cmake (@('-S','temp/vendor/libg722','-B','temp/build/libg722') + $common + @('-DENABLE_SHARED_LIB=OFF','-DENABLE_STATIC_LIB=ON','-DG722_BUILD_TEST_PROGRAMS=OFF','-DBUILD_TESTING=OFF'))
Run cmake @('--build','temp/build/libg722','--parallel','8')
Run cmake @('--install','temp/build/libg722')
Run cmake (@('-S','temp/vendor/re','-B','temp/build/re','-G','Ninja','-DCMAKE_BUILD_TYPE=Release','-DCMAKE_EXE_LINKER_FLAGS=/Brepro bcrypt.lib','-DCMAKE_STATIC_LINKER_FLAGS=/Brepro',"-DCMAKE_INSTALL_PREFIX=$prefix",'-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded','-DLIBRE_BUILD_SHARED=OFF','-DLIBRE_BUILD_STATIC=ON','-DUSE_MBEDTLS=OFF','-DUSE_BFCP=OFF','-DUSE_PCP=OFF','-DUSE_RTMP=OFF') + $tls + $tlsFlags)
Run cmake @('--build','temp/build/re','--parallel','8')
Run cmake @('--install','temp/build/re')
# baresip would otherwise embed the absolute install prefix as its default sound
# and module directories. KSIP writes audio_path itself and links every module
# statically, so fixed relative names are enough.
# KSIP's own code (src-native/CMakeLists.txt), compiled with the clang-cl the
# pinned WebRTC brings along, the one its build uses, so that all of KSIP's C
# and C++ goes through one compiler whose warnings and checks the rest can
# build on; baresip and the libraries stay with MSVC, as released. The
# archiver stays MSVC's lib.exe. clang-cl has no /d1trimfile; its prefix map
# drops the repository path from __FILE__ the same way, in both spellings a
# path may come in.
$webrtc = if ($env:KSIP_WEBRTC_SOURCE) { $env:KSIP_WEBRTC_SOURCE } else { "$root/temp/w/src" }
$clang = "$($webrtc.Replace('\','/'))/third_party/llvm-build/Release+Asserts/bin/clang-cl.exe"
if (!(Test-Path -LiteralPath $clang)) { throw "The clang-cl the pinned WebRTC brings is missing: $clang" }
$ksipFlags = "/O2 /DNDEBUG /DNOCRYPT /Brepro /clang:-ffile-prefix-map=$root/= /clang:-ffile-prefix-map=$($root.Replace('/','\'))\="
# The archiver and the linker stay MSVC's, named outright: for a clang
# compiler CMake takes LLVM's llvm-lib and lld-link when a machine has them
# (the GitHub runner has LLVM installed), and llvm-lib knows no /Brepro.
$lib = (Get-Command lib.exe).Source
$link = (Get-Command link.exe).Source
# The compile commands are for scripts/test/clang-tidy.ps1, which checks
# each file with the flags it was built with.
Run cmake (@('-S','src-native','-B','temp/build/ksip','-G','Ninja','-DCMAKE_BUILD_TYPE=Release','-DCMAKE_EXPORT_COMPILE_COMMANDS=ON',"-DCMAKE_C_COMPILER=$clang","-DCMAKE_CXX_COMPILER=$clang","-DCMAKE_C_FLAGS_RELEASE=$ksipFlags","-DCMAKE_CXX_FLAGS_RELEASE=$ksipFlags",'-DCMAKE_STATIC_LINKER_FLAGS=/Brepro',"-DCMAKE_AR=$lib","-DCMAKE_LINKER=$link","-DCMAKE_INSTALL_PREFIX=$prefix",'-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded',"-DKSIP_NATIVE=$prefix","-DBARESIP_SOURCE=$root/temp/vendor/baresip") + $tls)
Run cmake @('--build','temp/build/ksip','--parallel','8')
Run cmake @('--install','temp/build/ksip')
$paths = @('-DSHARE_PATH=share/baresip','-DMOD_PATH=lib/baresip/modules')
Run cmake (@('-S','temp/vendor/baresip','-B','temp/build/baresip','-G','Ninja','-DCMAKE_BUILD_TYPE=Release','-DCMAKE_EXE_LINKER_FLAGS=/Brepro bcrypt.lib','-DCMAKE_STATIC_LINKER_FLAGS=/Brepro',"-DCMAKE_INSTALL_PREFIX=$prefix", "-DCMAKE_PREFIX_PATH=$prefix",'-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded','-DSTATIC=ON','-DMODULES=account;menu;ksip_ctrl;g711;libg722;opus;srtp;dtls_srtp;ksip_audio;aufile;auconv;auresamp;ksip_audio_filter;ksip',"-DLIBG722_INCLUDE_DIR=$prefix/include","-DLIBG722_LIBRARY=$prefix/lib/g722_static.lib","-DOPUS_INCLUDE_DIR=$prefix/include","-DOPUS_LIBRARY=$prefix/lib/opus.lib","-DKSIP_NATIVE=$prefix") + $paths + $tls + $tlsFlags)
Run cmake @('--build','temp/build/baresip','--parallel','8')
Run cmake @('--install','temp/build/baresip')
