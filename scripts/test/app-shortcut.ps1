# Check the global shortcuts and what an incoming call does while in the tray.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$key='HKCU:\Software\KashiharaCity\ksip\Test\test-ui-ksip'
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;using System.Runtime.InteropServices;
public static class KsipKeys {
    [DllImport("user32.dll")] public static extern bool RegisterHotKey(IntPtr window,int id,uint modifiers,uint key);
    [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr window,int id);
}
'@
function Set-Setting([string]$name, $value) {
    $settings=(Get-ItemProperty -LiteralPath $key).Settings | ConvertFrom-Json
    $settings | Add-Member -NotePropertyName $name -NotePropertyValue $value -Force
    Set-ItemProperty -LiteralPath $key -Name 'Settings' -Value ($settings | ConvertTo-Json -Compress)
}
function Get-Setting([string]$name) {
    ((Get-ItemProperty -LiteralPath $key).Settings | ConvertFrom-Json).$name
}
function Send-Shortcut([string]$keys) {
    # A global shortcut answers wherever the keys are typed, so they go to
    # whatever holds the foreground, exactly as they would from another app.
    [System.Windows.Forms.SendKeys]::SendWait($keys)
}
$app=$null;$peer=$null;$caller=$null;$held=$false
try {
    $peer=Start-KsipFixturePeer 'peer' "$root/temp/build/ksip-ui"
    $end=[DateTime]::UtcNow.AddSeconds(25)
    while(!(Test-Path "$root/temp/build/ksip-ui/ready") -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(!(Test-Path "$root/temp/build/ksip-ui/ready")){throw 'SIP peer not ready'}
    Start-KsipProfile
    Set-Setting 'shortcut_window' 'CONTROL+ALT+F9'
    Set-Setting 'shortcut_call' 'CONTROL+ALT+F10'
    Set-Setting 'incoming_action' 'notify'
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    $window=$app.MainWindowHandle
    if(!$window){throw 'The window was never shown'}

    # The keys saved before the app started are registered while it starts.
    Send-Shortcut '^%{F9}'
    Wait-Visible $window $false
    'PASS: ショートカットでウィンドウをタスクトレイへしまう'
    Send-Shortcut '^%{F9}'
    Wait-Visible $window $true
    Wait-Class 'registration' 'reg-register_ok'
    'PASS: 同じショートカットでウィンドウを出す'

    # Windows looks the application identifier up here before it shows a toast.
    $aumid=Get-ItemProperty -LiteralPath 'HKCU:\Software\Classes\AppUserModelId\local.ksip.client' -ErrorAction SilentlyContinue
    if($aumid.DisplayName -ne 'KSIP'){throw "The notification identifier is not registered: $($aumid.DisplayName)"}
    'PASS: 通知用のAUMIDを登録している'

    # In the tray, a call is announced instead of pulling the window forward.
    Send-Shortcut '^%{F9}'
    Wait-Visible $window $false
    $caller=Start-KsipFixturePeer 'caller' "$root/temp/build/ksip-ui"
    Wait-KsipLog 'app' 'NOTIFY_INCOMING_SHOWN' | Out-Null
    if(Test-Visible $window){throw 'The window came out although the setting says notify'}
    'PASS: タスクトレイにいるときの着信は通知だけで、窓は出てこない'

    # One key answers while it rings, and hangs up afterwards. Answering brings
    # the window back, because the call is now the thing being used.
    Send-Shortcut '^%{F10}'
    Wait-Visible $window $true
    Wait-Class 'line-1' 'call-established'
    'PASS: ショートカットで応答し、ウィンドウが出てくる'
    Send-Shortcut '^%{F10}'
    Wait-Class 'line-1' 'call-idle'
    'PASS: 同じショートカットで通話を切る'
    Set-Content "$root/temp/build/ksip-ui/caller-done" 'done'
    if($caller -and !$caller.HasExited){$caller.WaitForExit(10000) | Out-Null}
    $caller=$null

    # What the settings dialog refuses, and what it accepts.
    Click-Id 'settings-button';Wait-Id 'save-settings' | Out-Null
    $field=Value-Id 'shortcut_window'
    $field.SetValue('SHIFT F2')
    Click-Id 'save-settings';Wait-Text 'settings-error' '記法が正しくありません' | Out-Null
    'PASS: 書き方が違うショートカットは断る'
    if(![KsipKeys]::RegisterHotKey([IntPtr]::Zero,1,0x0003,0x7B)){throw 'The test could not take CTRL+ALT+F12'}
    $held=$true
    $field.SetValue('CONTROL+ALT+F12')
    Click-Id 'save-settings';Wait-Text 'settings-error' 'ほかのアプリで使用されていない' | Out-Null
    'PASS: 他のアプリが使っているキーは断る'
    [KsipKeys]::UnregisterHotKey([IntPtr]::Zero,1) | Out-Null
    $held=$false
    # A refused save leaves the keys that were working in place.
    Send-Shortcut '^%{F9}'
    Wait-Visible $window $false
    Send-Shortcut '^%{F9}'
    Wait-Visible $window $true
    'PASS: 保存を断ったあとも元のショートカットが残る'
    $field=Value-Id 'shortcut_window'
    $field.SetValue('CONTROL+ALT+F11')
    Click-Id 'save-settings'
    $end=[DateTime]::UtcNow.AddSeconds(20)
    while((Find-Id 'save-settings') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(Find-Id 'save-settings'){throw 'Settings did not save'}
    Wait-Class 'registration' 'reg-register_ok'
    if((Get-Setting 'shortcut_window') -ne 'CONTROL+ALT+F11'){throw 'The new shortcut was not stored'}
    Send-Shortcut '^%{F11}'
    Wait-Visible $window $false
    Send-Shortcut '^%{F11}'
    Wait-Visible $window $true
    'PASS: 設定画面で変えたショートカットがすぐ効く'
    # The key that was replaced is no longer anyone's.
    Send-Shortcut '^%{F9}'
    Start-Sleep -Seconds 2
    if(!(Test-Visible $window)){throw 'The old shortcut is still registered'}
    'PASS: 古いショートカットは解除される'

    @{version=$version;togglesWindow=$true;answersAndHangsUp=$true;notifiesInTray=$true;
      registersNotificationIdentifier=$true;rejectsBadKey=$true;rejectsTakenKey=$true;keepsKeysWhenRefused=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-shortcut-v$version.json"
    'PASS: real KSIP driven through its global shortcuts'
} finally {
    if($held){[KsipKeys]::UnregisterHotKey([IntPtr]::Zero,1) | Out-Null}
    if($caller){Set-Content "$root/temp/build/ksip-ui/caller-done" 'done'
        if(!$caller.HasExited){$caller.WaitForExit(10000) | Out-Null}}
    Stop-KsipApp $app
    Stop-KsipEngine
    Set-Content "$root/temp/build/ksip-ui/done" 'done'
    if($peer -and !$peer.HasExited){$peer.WaitForExit(10000) | Out-Null}
    Stop-KsipProfile
}
