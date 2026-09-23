# Check the shipped exe: no external DLLs, singletons, child engine cleanup.
param([string]$Folder = "$PSScriptRoot/../../release")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/../dev-env.ps1"
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$version=Get-KsipVersion
$exe=Get-KsipReleaseExe $Folder
# The folder holds the executables plus whatever the app writes while running.
# Copies of earlier versions are kept there on purpose, for comparison.
$allowedFiles='ksip.exe','call-history.json','ksip-log.jsonl','ksip-notification.ico'
$allowedPatterns='^ksip-v[0-9]+\.[0-9]+\.[0-9]+\.exe$'
$allowedDirs='profile','recordings'
function Assert-FolderContents([string]$stage){
    $unexpected=@(Get-ChildItem -LiteralPath $Folder -File | Where-Object { $allowedFiles -notcontains $_.Name -and $_.Name -notmatch $allowedPatterns })
    $unexpected+=@(Get-ChildItem -LiteralPath $Folder -Directory | Where-Object { $allowedDirs -notcontains $_.Name })
    if($unexpected){throw "Unexpected files beside the executable ($stage): $(($unexpected | Select-Object -ExpandProperty Name) -join ', ')"}
}
Assert-FolderContents 'before run'
$imports=(& dumpbin /DEPENDENTS $exe) -join "`n"
if($LASTEXITCODE -ne 0){throw 'dumpbin failed'}
if($imports -match '(?i)(VCRUNTIME|MSVCP|webrtc|audio-devices|baresip|WebView2Loader)\S*\.dll'){throw 'External runtime/native DLL remains'}
$imports | Set-Content "$root/temp/build/single-exe-imports.txt"
Start-KsipProfile
$oldTarget=$env:KSIP_CREDENTIAL_TARGET
$app=$null;$child=$null;$duplicate=$null;$duplicateEngine=$null
try {
    $app=Start-Process $exe -WindowStyle Hidden -PassThru
    $deadline=[DateTime]::UtcNow.AddSeconds(25)
    $registered=$false
    do {
        Start-Sleep -Milliseconds 200;$app.Refresh()
        if($app.HasExited){throw 'App exited'}
        if($app.MainWindowHandle){
            Set-KsipWindow $app
            $registered=Test-Class 'registration' 'reg-register_ok'
        }
    } while(!$registered -and [DateTime]::UtcNow -lt $deadline)
    if(!$registered){throw 'Single exe did not register'}
    $children=@(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($app.Id)" | Where-Object { $_.CommandLine -match '--engine' })
    if($children.Count -ne 1 -or $children[0].ExecutablePath -ne $exe){throw 'Engine is not the same executable child'}
    $child=Get-Process -Id $children[0].ProcessId
    $duplicate=Start-Process $exe -WindowStyle Hidden -PassThru
    if(!$duplicate.WaitForExit(5000)){throw 'GUI singleton failed'}
    $env:KSIP_CREDENTIAL_TARGET='KSIP/Test/test-ui-ksip'
    $profile="$root/temp/build/test-ui-ksip/profile"
    $duplicateEngine=Start-Process $exe -ArgumentList '--engine',$app.Id,'-f',('"'+$profile+'"') -WindowStyle Hidden -PassThru
    if(!$duplicateEngine.WaitForExit(5000) -or $duplicateEngine.ExitCode -ne 4){throw 'Engine singleton failed'}
    $app.Kill()
    if(!$child.WaitForExit(7000)){throw 'Engine survived parent termination'}
    Assert-FolderContents 'after run'
    @{version=$version;singleExeFolder=$true;noExternalCrtOrAecDll=$true;autoRegister=$true;sameExecutableChild=$true;guiSingleton=$true;engineSingleton=$true;parentTerminationStopsChild=$true} | ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/single-exe-v$version.json"
    'PASS: one exe, static runtime/AEC, registration, GUI/engine singletons, parent termination cleanup'
} finally {
    foreach($owned in @($duplicateEngine,$duplicate,$app,$child)){if($owned){$owned.Refresh();if(!$owned.HasExited){$owned.Kill()}}}
    Stop-KsipProfile
    $env:KSIP_CREDENTIAL_TARGET=$oldTarget
}
