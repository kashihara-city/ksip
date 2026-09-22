# Check that a saved microphone that is not there is replaced by the default without being overwritten, and comes back on refresh.
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
function Get-SavedMicrophone { ((Get-ItemProperty -LiteralPath $key).Settings | ConvertFrom-Json).microphone }
function Set-SavedMicrophone([string]$value) {
    $settings=(Get-ItemProperty -LiteralPath $key).Settings | ConvertFrom-Json
    $settings | Add-Member -NotePropertyName 'microphone' -NotePropertyValue $value -Force
    Set-ItemProperty -LiteralPath $key -Name 'Settings' -Value ($settings | ConvertTo-Json -Compress)
}
$app=$null
try {
    Start-KsipProfile 'missing-device'
    $missing=Get-SavedMicrophone
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    # The engine runs on the default microphone, the notice says so, and the
    # saved choice is untouched.
    $config=Get-Content $configPath -Raw
    if($config -match [regex]::Escape("audio_source ksip_audio,$missing")){throw 'The engine was given the missing microphone'}
    Wait-Text 'microphone-volume-status' '既定のデバイスを使用中' | Out-Null
    if((Get-SavedMicrophone) -ne $missing){throw 'The saved microphone was overwritten'}
    'PASS: 保存したマイクが無ければ既定で動き、設定は上書きしない'
    # Once the saved device exists again, refreshing brings the engine back to it.
    Set-SavedMicrophone $real.id
    Click-Id 'settings-button';Wait-Id 'refresh-devices' | Out-Null
    Click-Id 'refresh-devices'
    $end=[DateTime]::UtcNow.AddSeconds(25)
    do {
        Start-Sleep -Milliseconds 300
        $config=Get-Content $configPath -Raw -ErrorAction SilentlyContinue
    } while(($config -notmatch [regex]::Escape("audio_source ksip_audio,$($real.id)")) -and [DateTime]::UtcNow -lt $end)
    if($config -notmatch [regex]::Escape("audio_source ksip_audio,$($real.id)")){throw 'Refresh did not restart the engine on the saved microphone'}
    Click-Id 'close-settings'
    Wait-Class 'registration' 'reg-register_ok'
    # An empty status element leaves the accessibility tree, so "not there" is "no notice".
    function Get-FallbackNotice { $node=Find-Id 'microphone-volume-status'; if($node){$node.Current.Name}else{''} }
    $end=[DateTime]::UtcNow.AddSeconds(10)
    while(((Get-FallbackNotice) -match '既定のデバイスを使用中') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 300}
    if((Get-FallbackNotice) -match '既定のデバイスを使用中'){throw 'The fallback notice stayed after the refresh'}
    'PASS: デバイスが戻ったら「音声デバイスを更新」で保存したマイクに戻る'
    @{version=$version;fallbackWithoutOverwrite=$true;refreshRestoresSavedDevice=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-devices-v$version.json"
    'PASS: real KSIP audio device fallback and refresh'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
