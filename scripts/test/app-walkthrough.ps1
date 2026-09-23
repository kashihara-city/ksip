# Drive the real app: registration, two lines, transfer, recording, tray, exit.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Windows.Forms
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
Start-KsipProfile
$app=$null;$peer=$null
try {
    $peer=Start-KsipFixturePeer 'peer' "$root/temp/build/ksip-ui"
    $end=[DateTime]::UtcNow.AddSeconds(25)
    while(!(Test-Path "$root/temp/build/ksip-ui/ready") -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(!(Test-Path "$root/temp/build/ksip-ui/ready")){throw 'SIP peer not ready'}
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    Wait-Id 'refresh-devices' | Out-Null
    Wait-Id 'open-sound-control' | Out-Null
    foreach($id in @('microphone-volume','speaker-volume')){
        $range=(Wait-Id $id).GetCurrentPattern([Windows.Automation.RangeValuePattern]::Pattern)
        if($range.Current.Maximum -ne 200){throw "$id maximum is $($range.Current.Maximum)"}
    }
    $app.Id | Set-Content "$root/temp/build/gui-test-pid.txt"
    & "$PSScriptRoot/app-screenshot.ps1" | Out-Null
    Click-Id 'record';Wait-Class 'record' 'active'
    $extension=(Get-Content "$root/temp/build/ksip-ui/peer-extension.txt" -Raw).Trim()
    (Value-Id 'target').SetValue($extension)
    Click-Id 'dial';Wait-Class 'line-1' 'call-established'
    # The footer names the codec the call settled on, and the header the transport.
    Wait-Text 'transport-label' '^UDP$' | Out-Null
    Wait-Text 'codec-label' 'Opus|G\.7' | Out-Null
    Wait-Text 'codec-label' '暗号化なし' | Out-Null
    $aecFlow=Wait-Id 'aec-metrics'
    $historyTab=Wait-Id 'history-tab'
    if($aecFlow.Current.BoundingRectangle.Bottom -gt $historyTab.Current.BoundingRectangle.Top){throw 'AEC diagnostics overlap the history tabs'}
    Click-Id 'line-2';Wait-Class 'line-2' 'call-idle'
    (Value-Id 'target').SetValue($extension)
    Click-Id 'dial';Wait-Class 'line-2' 'call-established'
    Click-Id 'line-1';Wait-Class 'line-1' 'call-established'
    Click-Id 'line-2';Wait-Class 'line-2' 'call-established'
    Wait-Class 'record-status' 'recording'
    Start-Sleep -Seconds 2
    Click-Id 'record';Wait-NoClass 'record' 'active'
    # The engine module reports the outcome as a name of its own.
    Click-Id 'transfer';Wait-Class 'transfer-status' 'transfer-done'
    # History and logs are fetched apart from the snapshot, so both must still fill in.
    Wait-Text 'call-history' '\d+/\d+ \d+:\d+' | Out-Null
    # The recorded call can be played from its row, and the file is named after
    # when it started and whom it was with. Playing would open another program,
    # so only the button and the file are checked.
    $play=New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::NameProperty,'録音を再生')
    if(!(Wait-Id 'call-history').FindFirst([Windows.Automation.TreeScope]::Descendants,$play)){throw 'The recorded call has no play button'}
    $wav=@(Get-ChildItem "$root/temp/build/test-ui-ksip/recordings" -Filter '*.wav' | Sort-Object LastWriteTime | Select-Object -Last 1)
    if(!$wav -or $wav[0].Name -notmatch "^\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}_$extension\.wav$"){throw "The recording is named $($wav[0].Name)"}
    'PASS: 録音した通話に「録音を再生」が出て、ファイル名は日時と相手番号'
    # One box narrows both panels: the history by number, the log by tag and
    # words, and says so while it does.
    Focus-Id 'panel-filter'
    [System.Windows.Forms.SendKeys]::SendWait('zzz')
    Wait-Text 'call-history' 'フィルターに一致する通話はありません' | Out-Null
    Wait-Id 'filter-active' | Out-Null
    [System.Windows.Forms.SendKeys]::SendWait('^a{BACKSPACE}'+$extension)
    Wait-Text 'call-history' '\d+/\d+ \d+:\d+' | Out-Null
    Click-Id 'logs-tab';Wait-Text 'logs' 'CALL_ESTABLISHED' | Out-Null
    # The words are matched regardless of case; a line about another event goes.
    Focus-Id 'panel-filter'
    [System.Windows.Forms.SendKeys]::SendWait('^a{BACKSPACE}register_ok')
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Text-Id 'logs') -match 'CALL_ESTABLISHED' -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    if((Text-Id 'logs') -match 'CALL_ESTABLISHED' -or (Text-Id 'logs') -notmatch 'REGISTER_OK'){throw "The log filter did not narrow the lines: $((Text-Id 'logs').Substring(0,[Math]::Min(200,(Text-Id 'logs').Length)))"}
    [System.Windows.Forms.SendKeys]::SendWait('^a{BACKSPACE}')
    Wait-Text 'logs' 'CALL_ESTABLISHED' | Out-Null
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Find-Id 'filter-active') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    if(Find-Id 'filter-active'){throw 'The filter label is still shown with an empty box'}
    'PASS: 入力欄で通話履歴は番号、ログは属性と本文で絞り込め、絞り込み中と出る'
    Click-Id 'history-tab'
    # The number can be taken to another application. The button sits in the row,
    # and its name ends with the number it copies.
    $button=New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Button)
    $copy=(Wait-Id 'call-history').FindFirst([Windows.Automation.TreeScope]::Descendants,$button)
    if(!$copy){throw 'The history row has no copy button'}
    $number=($copy.Current.Name -split ' ')[-1]
    Set-Clipboard -Value 'まだコピーしていません'
    Press $copy 'コピー'
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Get-Clipboard) -ne $number -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    if((Get-Clipboard) -ne $number){throw "Clipboard has $(Get-Clipboard) instead of $number"}
    # Selecting lines to copy holds the log still: the display says so, new
    # lines wait, Ctrl+A then Ctrl+C takes the log, and a click elsewhere lets
    # the lines in.
    Click-Id 'logs-tab';Wait-Text 'logs' 'REGISTER' | Out-Null
    Focus-Id 'logs'
    Wait-Id 'logs-paused' | Out-Null
    $held=Text-Id 'logs'
    Set-Clipboard -Value 'まだコピーしていません'
    [System.Windows.Forms.SendKeys]::SendWait('^a')
    Start-Sleep -Milliseconds 300
    [System.Windows.Forms.SendKeys]::SendWait('^c')
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Get-Clipboard) -notmatch '\d+/\d+ \d+:\d+:\d+ \[' -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    if((Get-Clipboard) -notmatch '\d+/\d+ \d+:\d+:\d+ \['){throw "Clipboard has $(Get-Clipboard) instead of log lines"}
    Click-Id 'reconnect';Start-Sleep -Seconds 3;Wait-Class 'registration' 'reg-register_ok'
    if(!(Find-Id 'logs-paused')){throw 'The log display resumed while the selection was still there'}
    if((Text-Id 'logs') -ne $held){throw 'The log display changed while it was held'}
    Click-At 'target'
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Find-Id 'logs-paused') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    if(Find-Id 'logs-paused'){throw 'The log display is still held after clicking elsewhere'}
    if((Text-Id 'logs') -eq $held){throw 'The lines that arrived meanwhile did not show'}
    'PASS: ログを選択している間は表示が止まり、コピーでき、離れると追いつく'
    # Clearing asks first, and names the list that is in front.
    Click-Id 'logs-tab';Wait-Text 'logs' 'REGISTER' | Out-Null
    Click-Id 'clear-panel';Wait-Text 'confirm' 'ログをクリアします' | Out-Null
    Click-Id 'confirm-ok'
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Text-Id 'logs') -match 'REGISTER' -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    if((Text-Id 'logs') -match 'REGISTER'){throw 'The log was not cleared'}
    Click-Id 'history-tab'
    Click-Id 'clear-panel';Wait-Text 'confirm' '通話履歴をクリアします' | Out-Null
    Click-Id 'confirm-ok'
    Wait-Class 'call-history' 'empty'
    Wait-Class 'line-2' 'call-idle'
    # Outside a call there is no codec to name.
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Text-Id 'codec-label') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    if(Text-Id 'codec-label'){throw 'The codec is still shown after the call ended'}
    Click-Id 'settings-button';Wait-Id 'save-settings' | Out-Null
    if((Value-Id 'password').Current.Value){throw 'Stored password returned to the UI'}
    # Switching the transport moves a default port, and leaves any other port alone.
    # The options are UDP first, then TLS.
    $port=Value-Id 'port'
    if($port.Current.Value -ne '5060'){throw "Unexpected port $($port.Current.Value)"}
    Select-Index 'transport' 1
    if((Value-Id 'port').Current.Value -ne '5061'){throw 'TLS did not move the port to 5061'}
    Select-Index 'transport' 0
    if((Value-Id 'port').Current.Value -ne '5060'){throw 'UDP did not move the port back to 5060'}
    $port.SetValue('5080')
    Select-Index 'transport' 1
    if((Value-Id 'port').Current.Value -ne '5080'){throw 'A port that is not the default must stay'}
    Select-Index 'transport' 0
    $port.SetValue('5060')
    $auth=Value-Id 'auth_user';$authId=$auth.Current.Value
    $auth.SetValue('invalid-test-account')
    Click-Id 'save-settings';Wait-Text 'settings-error' 'パスワードを入力してください' | Out-Null
    # The message the dialog showed must also be in the log, tagged as the web view.
    Wait-KsipLog 'ui' 'command: ACCOUNT_PASSWORD_REQUIRED' 5 | Out-Null
    $auth.SetValue($authId);Click-Id 'save-settings'
    $end=[DateTime]::UtcNow.AddSeconds(20)
    while((Find-Id 'save-settings') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(Find-Id 'save-settings'){throw 'Settings did not save/reconnect'}
    Wait-Class 'registration' 'reg-register_ok'
    # Closing hides the window but leaves the registered engine alive.
    $windowHandle=$app.MainWindowHandle
    $app.CloseMainWindow() | Out-Null;Start-Sleep -Milliseconds 800;$app.Refresh()
    if($app.HasExited){throw 'Close button exited instead of hiding to tray'}
    $child=Get-CimInstance Win32_Process -Filter "ParentProcessId=$($app.Id)" | Where-Object { $_.Name -eq 'ksip.exe' -and $_.CommandLine -match '--engine' }
    if(!$child){throw 'Engine stopped on window close'}
    # Restore the test window to exercise the explicit exit control.
    [KsipWindow]::ShowWindow($windowHandle,4) | Out-Null
    Click-Id 'quit';$app.WaitForExit(15000) | Out-Null
    if(!$app.HasExited){throw 'Explicit exit failed'}
    @{version=$version;autoRegister=$true;twoCalls=$true;lineSwitch=$true;aecDiagnostics=$true;historyAndLogsStream=$true;aecLayoutAboveHistory=$true;attendedTransfer=$true;receiveRecording=$true;settingsSaveReconnect=$true;passwordNotReturned=$true;changedIdentityRequiresPassword=$true;closeToTray=$true;explicitExit=$true} | ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/ui-ksip-v$version.json"
    'PASS: real KSIP UI auto-register, line 1/2, switching, recording, transfer, tray, exit'
} finally {
    Set-Content "$root/temp/build/ksip-ui/done" 'done'
    Stop-KsipApp $app
    if($peer){$peer.WaitForExit(10000)|Out-Null}
    Stop-KsipProfile
}
