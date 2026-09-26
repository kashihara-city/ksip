# Check what happens when the engine process dies under a recorded call: the call gets one history row, the exit is said once, the recording survives as an MP3, and a reconnect brings the phone back.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
$base="$root/temp/build/ksip-ui"
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
New-Item -ItemType Directory -Force $base | Out-Null
Start-KsipProfile
$app=$null;$peer=$null
$recordings="$root/temp/build/test-ui-ksip/recordings"
try {
    $peer=Start-KsipFixturePeer 'peer' $base
    $end=[DateTime]::UtcNow.AddSeconds(25)
    while(!(Test-Path "$base/ready") -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(!(Test-Path "$base/ready")){throw 'SIP peer not ready'}
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    $extension=(Get-Content "$base/peer-extension.txt" -Raw).Trim()
    (Value-Id 'target').SetValue($extension)
    Click-Id 'dial';Wait-Class 'line-1' 'call-established'
    Click-Id 'record';Wait-Class 'record-status' 'recording'
    Start-Sleep -Seconds 3
    Click-Id 'history-tab'
    $rowsBefore=([regex]::Matches((Text-Id 'call-history'),[regex]::Escape($extension))).Count
    $mp3Before=@(Get-ChildItem $recordings -Filter *.mp3 -ErrorAction SilentlyContinue).Count

    # The engine is this exe started with --engine by the app; it is killed, not asked to quit.
    $engine=Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'ksip.exe' -and $_.CommandLine -match '--engine' -and $_.ParentProcessId -eq $app.Id }
    if(!$engine){throw 'engine process not found'}
    Stop-Process -Id $engine.ProcessId -Force
    Wait-Text 'error' 'エンジンが終了しました' 15 | Out-Null
    Wait-Class 'line-1' 'call-idle'
    'PASS: エンジンの終了が窓に出て、通話は消える'

    # One row for the call that was up, from the last snapshot.
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while(([regex]::Matches((Text-Id 'call-history'),[regex]::Escape($extension))).Count -lt $rowsBefore+1 -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    $rowsAfter=([regex]::Matches((Text-Id 'call-history'),[regex]::Escape($extension))).Count
    if($rowsAfter -ne $rowsBefore+1){throw "history rows for the peer went $rowsBefore -> $rowsAfter"}
    'PASS: エンジンが死んだときの通話が履歴に 1 行残る'

    # Said once, not on every poll.
    Start-Sleep -Seconds 2
    $exits=@(Get-KsipLog | Where-Object { $_.code -eq 'ENGINE_EXITED' }).Count
    if($exits -ne 1){throw "the exit was logged $exits times"}
    'PASS: エンジンの終了は 1 回だけ記録される'

    # The recording the engine never closed is repaired and converted; no WAV is left behind.
    $end=[DateTime]::UtcNow.AddSeconds(20)
    while(((Get-ChildItem $recordings -Filter *.wav -ErrorAction SilentlyContinue).Count -gt 0 -or @(Get-ChildItem $recordings -Filter *.mp3 -ErrorAction SilentlyContinue).Count -lt $mp3Before+1) -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 300}
    if((Get-ChildItem $recordings -Filter *.wav -ErrorAction SilentlyContinue).Count -gt 0){throw 'a WAV of the killed recording was left unconverted'}
    if(@(Get-ChildItem $recordings -Filter *.mp3).Count -lt $mp3Before+1){throw 'the killed recording did not become an MP3'}
    'PASS: 閉じられなかった録音も MP3 になる'

    Click-Id 'reconnect';Wait-Class 'registration' 'reg-register_ok' 20
    'PASS: 再接続で登録に戻る'
    @{version=$version;oneHistoryRow=$true;exitLoggedOnce=$true;recordingRecovered=$true;reconnects=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-engine-exit-v$version.json"
    'PASS: real KSIP survives its engine dying under a call'
} finally {
    if($peer){Set-Content "$base/done" 'done';$peer.WaitForExit(15000)|Out-Null;if(!$peer.HasExited){Stop-Process -Id $peer.Id -Force}}
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
