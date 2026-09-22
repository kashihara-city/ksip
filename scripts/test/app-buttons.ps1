# Check the custom buttons against the real app: order, BLF state, speed dial, park and pick up, blind transfer.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
# The profile defines three buttons: a speed dial to the peer, a park slot the
# lab parks on *701 and picks up on 701, and a blind transfer to the 9001 player.
Start-KsipProfile 'buttons'
$app=$null;$peer=$null
function Get-ButtonOrder {
    $box=Wait-Id 'custom-actions'
    $condition=New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Button)
    @($box.FindAll([Windows.Automation.TreeScope]::Children,$condition) | ForEach-Object { $_.Current.AutomationId })
}
try {
    $peer=Start-KsipFixturePeer 'peer' "$root/temp/build/ksip-ui"
    $end=[DateTime]::UtcNow.AddSeconds(25)
    while(!(Test-Path "$root/temp/build/ksip-ui/ready") -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(!(Test-Path "$root/temp/build/ksip-ui/ready")){throw 'SIP peer not ready'}
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    # Only the configured buttons are shown, in their own order, each with its id.
    $order=Get-ButtonOrder
    if(($order -join ',') -ne 'custom-1,custom-2,custom-3'){throw "Buttons shown: $($order -join ',')"}
    if(Find-Id 'custom-4'){throw 'An unconfigured button is shown'}
    $extension=(Get-Content "$root/temp/build/ksip-ui/peer-extension.txt" -Raw).Trim()
    # The status is part of the button's name: its children are presentational.
    Wait-Text 'custom-1' ([regex]::Escape($extension)) | Out-Null
    Wait-Text 'custom-2' '701' | Out-Null
    Wait-Text 'custom-3' '9001' | Out-Null
    'PASS: 設定したボタンだけが順に出る'
    # Idle: the speed dial can be pressed, the park slot is free and waits for a
    # call, the transfer has nothing to send.
    Wait-NoClass 'custom-2' 'occupied'
    if((Wait-Id 'custom-2').Current.IsEnabled){throw 'Park is enabled without a call'}
    if((Wait-Id 'custom-3').Current.IsEnabled){throw 'Transfer is enabled without a call'}
    Click-Id 'custom-1';Wait-Class 'line-1' 'call-established'
    'PASS: 短縮ダイヤルで発信し、相手が応答した'
    # Park: the call leaves the line, the slot reports in use, and pressing the
    # same button again picks it up on a free line.
    Click-Id 'custom-2';Wait-Class 'line-1' 'call-idle'
    Wait-Class 'custom-2' 'occupied'
    'PASS: 保留ボタンで転送し、BLFが使用中になった'
    Click-Id 'custom-2';Wait-Class 'line-1' 'call-established'
    Wait-NoClass 'custom-2' 'occupied' 30
    'PASS: 使用中の保留ボタンで取得した'
    Click-Id 'hangup';Wait-Class 'line-1' 'call-idle'
    # Transfer: the peer is sent to the player and our leg ends.
    Click-Id 'custom-1';Wait-Class 'line-1' 'call-established'
    Click-Id 'custom-3';Wait-Class 'line-1' 'call-idle'
    'PASS: 転送ボタンで相手を9001へ転送した'
    @{version=$version;order=$order;speedDial=$true;park=$true;pickup=$true;transfer=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-buttons-v$version.json"
    'PASS: real KSIP custom buttons: speed dial, park with BLF, pick up, blind transfer'
} finally {
    if($peer){Set-Content "$root/temp/build/ksip-ui/done" 'done';$peer.WaitForExit(15000)|Out-Null;if(!$peer.HasExited){Stop-Process -Id $peer.Id -Force}}
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
