# Check that a saved microphone that is not there is handed to the engine as it is, the default serving in its place and shown so, without the choice being overwritten; and that a choice made again reaches the running engine on refresh, without a restart.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$key='HKCU:\Software\KashiharaCity\ksip\Test\test-ui-ksip'
$configPath=Join-Path $env:TEMP 'ksip-profile/test-ui-ksip/config'
# A real microphone of this machine, read the way the app reads them.
$helper=Join-Path $root 'temp/cargo-target/debug/examples/audio-devices.exe'
if(!(Test-Path $helper)){throw 'Run scripts/test/rust.ps1 first: the device helper is missing'}
$real=(& $helper | ConvertFrom-Json) | Where-Object { $_.kind -eq 'microphone' } | Select-Object -First 1
if(!$real){throw 'This machine has no microphone to test with'}
function Get-SavedMicrophone { (Get-ItemProperty -LiteralPath $key).microphone }
function Set-SavedMicrophone([string]$value) {
    Set-ItemProperty -LiteralPath $key -Name 'microphone' -Value $value -Type String
}
$app=$null
try {
    Start-KsipProfile 'missing-device'
    $missing=Get-SavedMicrophone
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    # The engine is given the saved microphone as it is (its streams open the
    # default in its place), the notice says the default is in use, and the
    # saved choice is untouched.
    $config=Get-Content $configPath -Raw
    if($config -notmatch [regex]::Escape("audio_source ksip_audio,$missing")){throw 'The engine was not given the saved microphone as it is'}
    Wait-Text 'microphone-volume-status' '既定のデバイスを使用中' | Out-Null
    if((Get-SavedMicrophone) -ne $missing){throw 'The saved microphone was overwritten'}
    'PASS: 保存したマイクが無ければ既定で動き、設定は上書きしない'
    # A microphone chosen again (here outside the window) reaches the engine
    # on refresh. The button sits on the main screen; the settings dialog is modal and would
    # take the main screen out of the accessibility tree.
    # The running engine takes the device for its next call, without a restart;
    # the app writes down what it handed over.
    Set-SavedMicrophone $real.id
    $appLog=Join-Path $root 'temp/build/test-ui-ksip/ksip-log.jsonl'
    $before=@(Get-Content $appLog -ErrorAction SilentlyContinue).Count
    Click-Id 'refresh-devices'
    $end=[DateTime]::UtcNow.AddSeconds(25)
    do {
        Start-Sleep -Milliseconds 300
        $since=@(Get-Content $appLog -ErrorAction SilentlyContinue | Select-Object -Skip $before)
        $taken=$since | Where-Object { $_ -match 'ksip: microphone ' -and $_ -match [regex]::Escape($real.id) }
    } while(!$taken -and [DateTime]::UtcNow -lt $end)
    if(!$taken){throw 'Refresh did not hand the saved microphone to the engine'}
    if($since | Where-Object { $_ -match 'ua: stop all' }){throw 'Refresh restarted the engine instead of handing the device over'}
    Wait-Class 'registration' 'reg-register_ok'
    # An empty status element leaves the accessibility tree, so "not there" is "no notice".
    function Get-FallbackNotice { $node=Find-Id 'microphone-volume-status'; if($node){$node.Current.Name}else{''} }
    $end=[DateTime]::UtcNow.AddSeconds(10)
    while(((Get-FallbackNotice) -match '既定のデバイスを使用中') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 300}
    if((Get-FallbackNotice) -match '既定のデバイスを使用中'){throw 'The fallback notice stayed after the refresh'}
    'PASS: 設定し直したマイクは「音声デバイスを更新」で、起動し直さずにエンジンへ渡る'
    # In a call the same button is the way back when a device notice was
    # missed: it must be enabled, the engine is handed the devices without a
    # restart, and the call goes on.
    $playback=(Get-KsipLab).numbers.playback
    (Value-Id 'target').SetValue($playback)
    Click-Id 'dial';Wait-Class 'line-1' 'call-established'
    $before=@(Get-Content $appLog -ErrorAction SilentlyContinue).Count
    Click-Id 'refresh-devices'
    $end=[DateTime]::UtcNow.AddSeconds(25)
    do {
        Start-Sleep -Milliseconds 300
        $since=@(Get-Content $appLog -ErrorAction SilentlyContinue | Select-Object -Skip $before)
        $taken=$since | Where-Object { $_ -match 'ksip: microphone ' -and $_ -match [regex]::Escape($real.id) }
    } while(!$taken -and [DateTime]::UtcNow -lt $end)
    if(!$taken){throw 'Refresh in a call did not hand the saved microphone to the engine'}
    if($since | Where-Object { $_ -match 'ua: stop all' }){throw 'Refresh in a call restarted the engine'}
    Wait-Class 'line-1' 'call-established'
    Click-Id 'hangup';Wait-Class 'line-1' 'call-idle'
    'PASS: 通話中でも「音声デバイスを更新」が押せ、エンジンを起動し直さずに機器を渡し、通話は続く'
    @{version=$version;fallbackWithoutOverwrite=$true;refreshRestoresSavedDevice=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-devices-v$version.json"
    'PASS: real KSIP audio device fallback and refresh'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
