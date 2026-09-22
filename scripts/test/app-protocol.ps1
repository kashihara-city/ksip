# Drive the app through ksip: links: dial, answer, hang up, show and quit.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$exe="$Folder/ksip.exe"
$key='HKCU:\Software\KashiharaCity\ksip\Test\test-ui-ksip'
function Send-Link([string]$command) {
    # A link always starts a new process, which hands the command over and exits.
    $sender=Start-Process $exe -ArgumentList "/ksip=ksip:$command" -PassThru -WindowStyle Hidden
    if(!$sender.WaitForExit(30000)){throw "The link process did not exit: $command"}
    if($sender.ExitCode -ne 0){throw "The link failed ($($sender.ExitCode)): $command"}
}
function Restart-Connection {
    # The label still says registered while the engine is being restarted, so
    # the reconnect is given time before anything is sent to it.
    Click-Id 'reconnect'
    Start-Sleep -Seconds 3
    Wait-Class 'registration' 'reg-register_ok'
    Start-Sleep -Seconds 1
}
function Set-Policy([string]$name, [string]$value) {
    # A policy value sits beside the settings document, one value per name.
    Set-ItemProperty -LiteralPath $key -Name $name -Value $value
}
function Set-Setting([string]$name, $value) {
    $settings=(Get-ItemProperty -LiteralPath $key).Settings | ConvertFrom-Json
    $settings | Add-Member -NotePropertyName $name -NotePropertyValue $value -Force
    Set-ItemProperty -LiteralPath $key -Name 'Settings' -Value ($settings | ConvertTo-Json -Compress)
}
$app=$null;$peer=$null
try {
    $peer=Start-KsipFixturePeer 'peer' "$root/temp/build/ksip-ui"
    $end=[DateTime]::UtcNow.AddSeconds(25)
    while(!(Test-Path "$root/temp/build/ksip-ui/ready") -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(!(Test-Path "$root/temp/build/ksip-ui/ready")){throw 'SIP peer not ready'}
    Start-KsipProfile
    Set-Setting 'browser_dial_confirm' $true
    $app=Start-KsipApp $exe "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'

    # A link dials, and asks first while the confirmation is on.
    Send-Link '9001'
    Wait-Text 'confirm' '9001 に発信しますか' | Out-Null
    Click-Id 'confirm-ok'
    Wait-Class 'line-1' 'call-established'
    'PASS: リンクから確認を経て発信した'
    Send-Link 'HANGUP'
    Wait-Class 'line-1' 'call-idle'
    'PASS: リンクから切断した'

    # A link that arrives while the app sits in the tray brings the window out,
    # because the question would otherwise be asked where nobody can see it.
    $window=$app.MainWindowHandle
    $app.CloseMainWindow() | Out-Null
    Wait-Visible $window $false
    Send-Link '9001'
    Wait-Visible $window $true
    Wait-Text 'confirm' '9001 に発信しますか' | Out-Null
    Click-Id 'confirm-ok'
    Wait-Class 'line-1' 'call-established'
    'PASS: タスクトレイにいても確認のためにウィンドウが出てくる'
    Send-Link 'HANGUP'
    Wait-Class 'line-1' 'call-idle'

    # Without the confirmation the call starts straight away, separators and all.
    Set-Setting 'browser_dial_confirm' $false
    Restart-Connection
    Send-Link '90-01'
    Wait-Class 'line-1' 'call-established'
    'PASS: 確認なしの設定では区切り文字を除いてそのまま発信した'
    Send-Link 'HANGUP'
    Wait-Class 'line-1' 'call-idle'

    # An incoming call can be answered by a link.
    $extension=(Get-Content "$root/temp/build/ksip-ui/peer-extension.txt" -Raw).Trim()
    $caller=Start-KsipFixturePeer 'caller' "$root/temp/build/ksip-ui"
    try {
        Wait-Class 'line-1' 'call-incoming'
        Send-Link 'ANSWER'
        Wait-Class 'line-1' 'call-established'
        'PASS: リンクから着信に応答した'
        Send-Link 'HANGUP'
        Wait-Class 'line-1' 'call-idle'
    } finally {
        Set-Content "$root/temp/build/ksip-ui/caller-done" 'done'
        if($caller -and !$caller.HasExited){$caller.WaitForExit(10000) | Out-Null}
    }

    Send-Link 'SHOWWINDOW'
    Wait-Class 'registration' 'reg-register_ok'
    'PASS: SHOWWINDOWでも窓は生きている'

    # The registration follows the setting, and is applied when the app starts.
    Set-Policy 'browser_integration' 'true'
    Stop-KsipApp $app; Stop-KsipEngine
    $app=Start-KsipApp $exe "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    $command=(Get-ItemProperty -LiteralPath 'HKCU:\Software\Classes\ksip\shell\open\command' -ErrorAction SilentlyContinue).'(default)'
    if($command -notlike "*$([IO.Path]::GetFullPath($exe))*"){throw "The protocol command is $command"}
    'PASS: 設定をONにするとksip:を登録する'
    Set-Policy 'browser_integration' 'false'
    Stop-KsipApp $app; Stop-KsipEngine
    $app=Start-KsipApp $exe "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    if(Test-Path 'HKCU:\Software\Classes\ksip'){throw 'The registration is still there'}
    'PASS: 設定をOFFにすると登録を消す'

    Send-Link 'APP_QUIT'
    if(!$app.WaitForExit(15000)){throw 'APP_QUIT did not close the app'}
    $app=$null
    'PASS: リンクから終了した'
    @{version=$version;dial=$true;answer=$true;hangup=$true;showWindow=$true;quit=$true;registers=$true;
      showsWindowToAsk=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-protocol-v$version.json"
    'PASS: real KSIP driven through ksip: links'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Set-Content "$root/temp/build/ksip-ui/done" 'done'
    if($peer -and !$peer.HasExited){$peer.WaitForExit(10000) | Out-Null}
    Stop-KsipProfile
}
