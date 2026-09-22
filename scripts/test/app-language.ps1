# Check that the settings switch the window's language, and that it comes back.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$key='HKCU:\Software\KashiharaCity\ksip\Test\test-ui-ksip'
# The options are: follow Windows, 日本語, English, 繁體中文.
function Set-Language([int]$index) {
    Click-Id 'settings-button';Wait-Id 'save-settings' | Out-Null
    Select-Index 'language' $index
    Click-Id 'save-settings'
    $end=[DateTime]::UtcNow.AddSeconds(20)
    while((Find-Id 'save-settings') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(Find-Id 'save-settings'){throw 'Settings did not save'}
    Wait-Class 'registration' 'reg-register_ok'
}
$app=$null;$peer=$null
try {
    $peer=Start-KsipFixturePeer 'peer' "$root/temp/build/ksip-ui"
    $end=[DateTime]::UtcNow.AddSeconds(25)
    while(!(Test-Path "$root/temp/build/ksip-ui/ready") -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(!(Test-Path "$root/temp/build/ksip-ui/ready")){throw 'SIP peer not ready'}
    Start-KsipProfile
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    Wait-Text 'dial' '^発信$' | Out-Null
    'PASS: 既定ではWindowsに合わせて日本語で出る'

    Set-Language 2
    Wait-Text 'dial' '^Call$' | Out-Null
    Wait-Text 'settings-button' '^Settings$' | Out-Null
    if(((Get-ItemProperty -LiteralPath $key).Settings | ConvertFrom-Json).language -ne 'en'){throw 'The language was not stored'}
    'PASS: 英語へ切り替わり、設定にも残る'

    Set-Language 3
    Wait-Text 'dial' '^撥號$' | Out-Null
    'PASS: 繁体字中国語へ切り替わる'

    # The engine reports names, so its words follow the window's language too.
    # Ending the second call is what makes it report one.
    $extension=(Get-Content "$root/temp/build/ksip-ui/peer-extension.txt" -Raw).Trim()
    foreach($line in 2,1){
        Click-Id "line-$line"
        (Value-Id 'target').SetValue($extension)
        Click-Id 'dial';Wait-Class "line-$line" 'call-established'
    }
    Click-Id 'hangup';Wait-Class 'line-1' 'call-idle'
    Wait-Text 'transfer-status' '保留中的通話' | Out-Null
    Click-Id 'line-2';Click-Id 'hangup';Wait-Class 'line-2' 'call-idle'
    'PASS: エンジンが返す結果も選んだ言語で出る'

    Set-Language 1
    Wait-Text 'dial' '^発信$' | Out-Null
    'PASS: 日本語へ戻せる'
    @{version=$version;followsWindows=$true;switchesToEnglish=$true;switchesToChinese=$true;
      enginePartsFollow=$true;switchesBack=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-language-v$version.json"
    'PASS: real KSIP switches language from the settings'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Set-Content "$root/temp/build/ksip-ui/done" 'done'
    if($peer -and !$peer.HasExited){$peer.WaitForExit(10000) | Out-Null}
    Stop-KsipProfile
}
