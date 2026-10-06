# clang-tidy over KSIP's own C++ (src-native), with the compile commands
# native.ps1 writes, so that each file is checked with the flags of the build.
# The checks are src-native/.clang-tidy; any finding fails. No SIP and no
# application.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../dev-env.ps1"
$root = (Split-Path (Split-Path $PSScriptRoot)).Replace('\','/')
Set-Location $root
# The clang-tidy of the clang-cl that builds src-native, beside it
# (scripts/deps/fetch-clang-tidy.py puts it there).
$webrtc = if ($env:KSIP_WEBRTC_SOURCE) { $env:KSIP_WEBRTC_SOURCE } else { "$root/temp/w/src" }
$tidy = "$($webrtc.Replace('\','/'))/third_party/llvm-build/Release+Asserts/bin/clang-tidy.exe"
if (!(Test-Path -LiteralPath $tidy)) { throw "clang-tidy is missing ($tidy): run python scripts/deps/fetch-clang-tidy.py" }
$build = "$root/temp/build/ksip"
if (!(Test-Path -LiteralPath "$build/compile_commands.json")) { throw "Run scripts/build/native.ps1 first: $build/compile_commands.json is missing" }
$files = @((Get-Content -LiteralPath "$build/compile_commands.json" -Raw | ConvertFrom-Json) | ForEach-Object { $_.file })
if (!$files.Count) { throw "$build/compile_commands.json names no file" }
$started = Get-Date
& $tidy -p $build --quiet @files
if ($LASTEXITCODE -ne 0) { throw "clang-tidy reported findings ($LASTEXITCODE)" }
Write-Host ("clang-tidy: {0} files of src-native are clean ({1:N0} s)" -f $files.Count, ((Get-Date) - $started).TotalSeconds)
