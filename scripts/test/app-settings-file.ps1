# Check the settings dialog's export and import: the saved settings go to a file, a file's values go into the dialog (not the machine's devices, nor values the other machine could not read), and only saving keeps them.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$work="$root/temp/build/settings-file"
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $work | Out-Null

# The standard file dialog, found by its title, given a path.
function Use-FileDialog([string]$title, [string]$path) {
    $name=New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::NameProperty,$title)
    $end=[DateTime]::UtcNow.AddSeconds(15)
    $dialog=$null
    while(!$dialog -and [DateTime]::UtcNow -lt $end){
        $dialog=[Windows.Automation.AutomationElement]::RootElement.FindFirst([Windows.Automation.TreeScope]::Descendants,$name)
        if(!$dialog){Start-Sleep -Milliseconds 200}
    }
    if(!$dialog){throw "No file dialog titled $title"}
    # Windows 11 does not let UIAutomation set the file name box's value: the
    # path is pasted (not typed, so that the input method cannot turn it into
    # something else), and Enter takes it. The keys go to the window in front,
    # and in the dialog to what has the focus, which is not always the box
    # (the file list, say, where Ctrl+A and Ctrl+V mean files): so the dialog
    # is brought to the front and the box chosen by its access key, Alt+N
    # (ファイル名(N)), each time; a dialog still open after that is tried
    # again, up to three times.
    Start-Sleep -Milliseconds 500
    Set-Clipboard -Value ([IO.Path]::GetFullPath($path))
    for($attempt=1; $attempt -le 3; $attempt++){
        Set-Foreground ([IntPtr]$dialog.Current.NativeWindowHandle) "The file dialog '$title'"
        [System.Windows.Forms.SendKeys]::SendWait('%n')
        Start-Sleep -Milliseconds 200
        [System.Windows.Forms.SendKeys]::SendWait('^a')
        [System.Windows.Forms.SendKeys]::SendWait('^v')
        Start-Sleep -Milliseconds 300
        [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
        $gone=[DateTime]::UtcNow.AddSeconds(5)
        while([Windows.Automation.AutomationElement]::RootElement.FindFirst([Windows.Automation.TreeScope]::Descendants,$name) -and [DateTime]::UtcNow -lt $gone){Start-Sleep -Milliseconds 200}
        if(![Windows.Automation.AutomationElement]::RootElement.FindFirst([Windows.Automation.TreeScope]::Descendants,$name)){return}
    }
    throw "The file dialog '$title' did not take the path"
}
function Open-Settings { Click-Id 'settings-button';Wait-Id 'save-settings' | Out-Null }
function Export-Saved {
    # What KSIP has saved, from the exe's own export of this test profile.
    $out="$work/saved.json"
    Remove-Item $out -ErrorAction SilentlyContinue
    # The exe is a windowed program: it is waited for explicitly.
    $export=Start-Process "$Folder/ksip.exe" -ArgumentList '--export-settings',"`"$out`"" -Wait -PassThru
    if($export.ExitCode -ne 0){throw "ksip.exe --export-settings ended with $($export.ExitCode)"}
    Get-Content -Raw -Encoding UTF8 $out | ConvertFrom-Json
}

$app=$null
try {
    Start-KsipProfile
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'

    # Export: the saved settings, in the same document as --export-settings.
    Open-Settings
    $exported="$work/exported.json"
    Click-Id 'export-settings'
    Use-FileDialog '保存済みの設定の書き出し先' $exported
    Wait-Text 'settings-file-note' ([regex]::Escape('exported.json')) | Out-Null
    $file=Get-Content -Raw -Encoding UTF8 $exported | ConvertFrom-Json
    if($file.format -ne 1){throw 'The export is not format 1'}
    if($file.settings.sip_port -ne 17560){throw "The export has sip_port $($file.settings.sip_port), not the saved 17560"}
    if(!$file.account.signed_in -or !$file.valid){throw "The export does not say the profile is ready to connect: $($file.error)"}
    if((Get-Content -Raw $exported) -match 'password'){throw 'The export mentions a password'}
    'PASS: 書き出しは保存済みの設定を CLI と同じ形でファイルにする（パスワードなし）'

    # Import into the dialog only: closing without saving keeps what was saved.
    $address=$file.account.server
    $other=$file | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    $other.account.server='pbx.example'
    $other | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 "$work/other-server.json"
    Click-Id 'import-settings'
    Use-FileDialog '読み込む設定ファイル' "$work/other-server.json"
    Wait-Text 'settings-file-note' '画面に読み込みました' | Out-Null
    if((Value-Id 'server').Current.Value -ne 'pbx.example'){throw 'The imported server is not in the dialog'}
    Click-Id 'close-settings'
    Open-Settings
    if((Value-Id 'server').Current.Value -ne $address){throw 'Closing without saving kept the imported server'}
    'PASS: 読み込みは画面に入れるだけで、保存せずに閉じれば元のまま'

    # Import and save: what the file names changes, what it leaves out stays,
    # and the machine's devices and what the other machine could not read are
    # passed over.
    $changed=$file | ConvertTo-Json -Depth 8 | ConvertFrom-Json
    $changed.settings.tray_after_call=15
    $changed.settings.aec_delay_ms=33
    $changed.settings.microphone='{0.0.1.00000000}.{00000000-0000-0000-0000-000000000000}'
    $changed.settings.agc=$true
    $changed.settings.register_interval='often'
    $changed.settings.buttons[1].title='Imported'
    $changed.unreadable=@('agc')
    $changed | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 "$work/changed.json"
    Click-Id 'import-settings'
    Use-FileDialog '読み込む設定ファイル' "$work/changed.json"
    Wait-Text 'settings-file-note' 'agc' | Out-Null
    Wait-Text 'settings-file-note' 'register_interval' | Out-Null
    Click-Id 'save-settings'
    $end=[DateTime]::UtcNow.AddSeconds(20)
    while((Find-Id 'save-settings') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(Find-Id 'save-settings'){throw 'The imported settings did not save'}
    Wait-Class 'registration' 'reg-register_ok'
    $saved=Export-Saved
    if($saved.settings.tray_after_call -ne 15 -or $saved.settings.aec_delay_ms -ne 33 -or $saved.settings.buttons[1].title -ne 'Imported'){throw 'The imported values were not saved'}
    if($saved.settings.microphone -ne $file.settings.microphone){throw 'The microphone of another machine was taken'}
    if($saved.settings.agc -ne $file.settings.agc){throw 'A value the other machine could not read was taken'}
    if($saved.settings.register_interval -ne $file.settings.register_interval){throw 'A value of the wrong type was taken'}
    if($saved.settings.sip_port -ne 17560 -or $saved.account.server -ne $address){throw 'What the file did not change did not stay'}
    'PASS: 保存すると読み込んだ値が残り、端末固有の値・元の端末で読めなかった値・型の違う値は入らない'
    'PASS: settings export and import'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
