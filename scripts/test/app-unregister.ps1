# Check that unregistering stops calls arriving, and that reconnecting brings them back.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
# The fixture keeps its own files in this folder, so the test looks there too.
$base="$root/temp/build/ksip-ui"
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
New-Item -ItemType Directory -Force $base | Out-Null
$script:caller=$null
function Start-Caller(){
    Remove-Item "$base/caller-done","$base/caller-ready","$base/caller-result.txt" -ErrorAction SilentlyContinue
    $script:caller=Start-KsipFixturePeer 'caller' $base
    $end=[DateTime]::UtcNow.AddSeconds(30)
    while(!(Test-Path "$base/caller-ready") -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(!(Test-Path "$base/caller-ready")){throw 'SIP caller did not place the call'}
}
function Stop-Caller(){
    if(!$script:caller){return $null}
    Set-Content "$base/caller-done" 'done'
    $script:caller.WaitForExit(20000) | Out-Null
    if(!$script:caller.HasExited){Stop-Process -Id $script:caller.Id -Force}
    $script:caller=$null
    if(Test-Path "$base/caller-result.txt"){(Get-Content "$base/caller-result.txt" -Raw).Trim()}else{'NO_RESULT'}
}
$app=$null
try {
    Start-KsipProfile
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'

    # Unregistering asks first, because nobody can reach this phone afterwards.
    Click-Id 'unregister'
    Wait-Text 'confirm' '着信しません' | Out-Null
    Click-Id 'confirm-ok'
    Wait-Class 'registration' 'reg-unregistered'
    # Nothing to unregister twice.
    if((Find-Id 'unregister').Current.IsEnabled){throw 'The unregister button is still enabled'}
    'PASS: 確認のうえ登録を解除する'

    # Where the phone is connected means nothing while it is not registered.
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Find-Id 'connection-server') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    if(Find-Id 'connection-server'){throw 'The server row is still shown'}
    if(Find-Id 'transport-label'){throw 'The transport tag is still shown'}
    Wait-Text 'registration' '着信しません' | Out-Null
    'PASS: 解除中はサーバーと通信方式を出さない'

    # A call placed while unregistered never reaches the window.
    Start-Caller
    Start-Sleep -Seconds 8
    if(Test-Class 'line-1' 'call-incoming'){throw 'A call arrived although the phone is unregistered'}
    $result=Stop-Caller
    if($result -eq 'ESTABLISHED'){throw "The caller reported $result while unregistered"}
    'PASS: 解除中は着信しない'

    # Reconnecting registers again, and calls arrive as before.
    Click-Id 'reconnect'
    Wait-Class 'registration' 'reg-register_ok'
    Wait-Text 'transport-label' '^(UDP|TLS)$' | Out-Null
    Start-Caller
    Wait-Class 'line-1' 'call-incoming'
    Click-Id 'answer';Wait-Class 'line-1' 'call-established'
    $result=Stop-Caller
    if($result -ne 'ESTABLISHED'){throw "The caller reported $result after reconnecting"}
    Wait-Class 'line-1' 'call-idle'
    'PASS: 再接続で登録し直し、着信できる'
    @{version=$version;unregisters=$true;noCallsWhileUnregistered=$true;reconnects=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-unregister-v$version.json"
    'PASS: real KSIP unregisters on request and comes back'
} finally {
    Stop-Caller | Out-Null
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
