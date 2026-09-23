# Check the custom buttons against the real app: order, BLF state, speed dial, park and pick up, blind transfer.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
# The profile defines five buttons: a speed dial to the peer, a park slot the
# lab parks on *701 and picks up on 701, a blind transfer to the 9001 player,
# the same transfer written as a full SIP URI in angle brackets, and a link.
# It also asks for the window to go to the tray ten seconds after a call.
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
    if(($order -join ',') -ne 'custom-1,custom-2,custom-3,custom-4,custom-5'){throw "Buttons shown: $($order -join ',')"}
    if(Find-Id 'custom-6'){throw 'An unconfigured button is shown'}
    $extension=(Get-Content "$root/temp/build/ksip-ui/peer-extension.txt" -Raw).Trim()
    # The status is part of the button's name: its children are presentational.
    Wait-Text 'custom-1' ([regex]::Escape($extension)) | Out-Null
    Wait-Text 'custom-2' '701' | Out-Null
    Wait-Text 'custom-3' '9001' | Out-Null
    Wait-Text 'custom-4' 'sip:9001@' | Out-Null
    # The link button is usable without a call; it is not pressed here, because
    # that would open a browser on the machine running the test.
    Wait-Text 'custom-5' 'example.invalid' | Out-Null
    if(!(Wait-Id 'custom-5').Current.IsEnabled){throw 'Link button is disabled'}
    'PASS: 設定したボタンだけが順に出る'
    # A button in the panel beside the phone: the window is twice as wide, the
    # panel holds the button, and it dials like any other.
    Wait-Text 'custom-7' '9001' | Out-Null
    $panel=Wait-Id 'extended-actions'
    if(!$panel.FindFirst([Windows.Automation.TreeScope]::Children,(New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::AutomationIdProperty,'custom-7')))){throw 'The panel button is not in the panel'}
    $width=(Get-KsipRoot).Current.BoundingRectangle.Width
    if($width -lt 900){throw "The window is only $width wide with a panel button set"}
    Click-Id 'custom-7';Wait-Class 'line-1' 'call-established'
    Click-Id 'hangup';Wait-Class 'line-1' 'call-idle'
    'PASS: 拡張ボタンは右側の欄に出て、窓が2倍の幅になり、発信できる'
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
    # The same, with the target written as a full URI: the engine passes it on.
    Click-Id 'custom-1';Wait-Class 'line-1' 'call-established'
    Click-Id 'custom-4';Wait-Class 'line-1' 'call-idle'
    'PASS: 完全なSIP URIを転送先にしたボタンでも転送できた'
    # Ten seconds after that last call ended, with no other call, the window is in the tray.
    $windowHandle=$app.MainWindowHandle
    Wait-Visible $windowHandle $false 25
    $app.Refresh();if($app.HasExited){throw 'The app exited instead of going to the tray'}
    'PASS: 通話が終わって設定した秒数でタスクトレイへ戻った'
    [KsipWindow]::ShowWindow($windowHandle,4) | Out-Null
    Wait-Visible $windowHandle $true 10
    # Do not disturb, last: the caller registers as the peer's account and
    # unseats it, so nothing may need the peer after this. While it is on, a
    # call never reaches the phone (the lab's voicemail takes it), and the
    # header says so.
    # Left without a title, the switch names itself in the window's language.
    Wait-Text 'custom-8' '^着信拒否' | Out-Null
    Click-Id 'custom-8';Wait-Class 'custom-8' 'dnd';Wait-Text 'registration' '着信拒否中' | Out-Null
    $base="$root/temp/build/ksip-ui"
    Remove-Item "$base/caller-done","$base/caller-ready","$base/caller-result.txt" -ErrorAction SilentlyContinue
    $caller=Start-KsipFixturePeer 'caller' $base
    try {
        $end=[DateTime]::UtcNow.AddSeconds(8)
        while([DateTime]::UtcNow -lt $end){if(Test-Class 'line-1' 'call-incoming'){throw 'A call rang through while do not disturb was on'};Start-Sleep -Milliseconds 200}
    } finally {
        Set-Content "$base/caller-done" 'done'
        if($caller -and !$caller.HasExited){$caller.WaitForExit(15000) | Out-Null}
    }
    Click-Id 'custom-8';Wait-NoClass 'custom-8' 'dnd'
    Wait-Text 'call-history' '着信拒否' | Out-Null
    'PASS: 着信拒否ボタンでONの間は着信が鳴らず、OFFで戻る'
    # Voicemail: a message left in the box (by a phone on the peer's account,
    # so also last) makes the button count one more and turn red.
    # The call refused above went to the box as well; its count arrives a moment later.
    Start-Sleep -Seconds 4
    Wait-Text 'custom-9' '^留守番電話 新着' | Out-Null
    $before=if((Text-Id 'custom-9') -match '新着 (\d+) 件'){[int]$Matches[1]}else{0}
    python -X utf8 "$PSScriptRoot/app_fixture.py" voicemail
    if($LASTEXITCODE -ne 0){throw 'Leaving a voicemail failed'}
    Wait-Text 'custom-9' ('新着 '+($before+1)+' 件') 30 | Out-Null
    Wait-Class 'custom-9' 'mwi-new'
    'PASS: 留守番電話ボタンに新着の件数が出て赤くなる'
    # Answered elsewhere, last as well: the lab's 7000 rings this phone and the
    # third account together, the third takes it, and the PBX cancels here
    # with a Reason header. The history says so in place of a missed call.
    # The history is read as soon as the ring ends: the window goes to the
    # tray ten seconds after a call in this profile.
    $group=Start-KsipFixturePeer 'group' $base
    try {
        Wait-Class 'line-1' 'call-incoming' 30
        Wait-Class 'line-1' 'call-idle' 30
        Wait-Text 'call-history' '他で応答' | Out-Null
    } finally {
        if($group -and !$group.HasExited){$group.WaitForExit(30000) | Out-Null}
        if(!$group.HasExited){Stop-Process -Id $group.Id -Force}
    }
    if($group.ExitCode -ne 0){throw 'The group call failed'}
    Wait-KsipLog 'event' 'CALL_CLOSED .*cause=200' | Out-Null
    'PASS: 他の電話が取った着信は履歴に「他で応答」と出る'
    @{version=$version;order=$order;speedDial=$true;park=$true;pickup=$true;transfer=$true;transferToUri=$true;linkButton=$true;trayAfterCall=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-buttons-v$version.json"
    'PASS: real KSIP custom buttons: speed dial, park with BLF, pick up, blind transfer'
} finally {
    if($peer){Set-Content "$root/temp/build/ksip-ui/done" 'done';$peer.WaitForExit(15000)|Out-Null;if(!$peer.HasExited){Stop-Process -Id $peer.Id -Force}}
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
