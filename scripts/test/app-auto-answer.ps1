# Check the automatic answer setting against a real incoming call.
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
$app=$null;$caller=$null
function Get-AutoAnswerToggle(){
    $node=Wait-Id 'auto_answer'
    $pattern=$null
    if(!$node.TryGetCurrentPattern([Windows.Automation.TogglePattern]::Pattern,[ref]$pattern)){throw 'Auto answer control is not a toggle'}
    $pattern
}
function Wait-AutoAnswerState([Windows.Automation.ToggleState]$state){
    # The checkbox is repainted by the page, so the toggle state lands a moment after the click.
    $end=[DateTime]::UtcNow.AddSeconds(10)
    do{if((Get-AutoAnswerToggle).Current.ToggleState -eq $state){return};Start-Sleep -Milliseconds 200}while([DateTime]::UtcNow -lt $end)
    throw "Automatic answering is not $state"
}
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
try {
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    # The stored setting starts off, so an incoming call must keep ringing on its own.
    Start-Caller
    Wait-Class 'line-1' 'call-incoming'
    Start-Sleep -Seconds 5
    if(!(Test-Class 'line-1' 'call-incoming')){throw 'Call was answered while automatic answering was off'}
    $offResult=Stop-Caller
    if($offResult -ne 'NOT_ESTABLISHED'){throw "Caller reported $offResult while automatic answering was off"}
    Wait-Class 'line-1' 'call-idle'
    'PASS: 自動応答OFFでは着信を自動で取らない'
    # Turn the setting on through the settings dialog, exactly as an operator would.
    Click-Id 'settings-button';Wait-Id 'save-settings' | Out-Null
    Wait-AutoAnswerState ([Windows.Automation.ToggleState]::Off)
    (Get-AutoAnswerToggle).Toggle()
    Wait-AutoAnswerState ([Windows.Automation.ToggleState]::On)
    Click-Id 'save-settings'
    $end=[DateTime]::UtcNow.AddSeconds(25)
    while((Find-Id 'save-settings') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(Find-Id 'save-settings'){throw 'Settings did not save/reconnect'}
    Wait-Class 'registration' 'reg-register_ok'
    # Nothing touches the 応答 button from here on.
    Start-Caller
    Wait-Class 'line-1' 'call-established'
    $onResult=Stop-Caller
    if($onResult -ne 'ESTABLISHED'){throw "Caller reported $onResult while automatic answering was on"}
    Wait-Class 'line-1' 'call-idle'
    'PASS: 自動応答ONで着信を自動応答し通話が成立する'
    # The setting must survive the reconnect and come back to the dialog.
    Click-Id 'settings-button';Wait-Id 'save-settings' | Out-Null
    Wait-AutoAnswerState ([Windows.Automation.ToggleState]::On)
    Click-Id 'close-settings'
    @{version=$version;defaultsOff=$true;ringsWhenOff=$true;answersWhenOn=$true;callerEstablished=$true;settingPersists=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/auto-answer-v$version.json"
    'PASS: real KSIP automatic answering, off/on through the settings dialog'
} finally {
    Stop-Caller | Out-Null
    if($app){$app.Refresh();if(!$app.HasExited){$node=Find-Id 'quit';if($node){$node.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()};$app.WaitForExit(10000)|Out-Null}}
    if($app -and !$app.HasExited){
        python -X utf8 "$PSScriptRoot/app_fixture.py" stop-engine
        Stop-Process -Id $app.Id -Force
    }
    python -X utf8 "$PSScriptRoot/app_fixture.py" cleanup
    $env:KSIP_TEST_PROFILE=$oldProfile
}
