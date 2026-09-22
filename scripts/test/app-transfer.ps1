# Check what a second call does to the first one, from either line.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$app=$null;$peer=$null
function Start-Call([int]$line, [string]$number) {
    Click-Id "line-$line"
    (Value-Id 'target').SetValue($number)
    Click-Id 'dial';Wait-Class "line-$line" 'call-established'
}
function Stop-Call([int]$line) {
    Click-Id "line-$line"
    Click-Id 'hangup';Wait-Class "line-$line" 'call-idle'
}
try {
    $peer=Start-KsipFixturePeer 'peer' "$root/temp/build/ksip-ui"
    $end=[DateTime]::UtcNow.AddSeconds(25)
    while(!(Test-Path "$root/temp/build/ksip-ui/ready") -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
    if(!(Test-Path "$root/temp/build/ksip-ui/ready")){throw 'SIP peer not ready'}
    Start-KsipProfile
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    $extension=(Get-Content "$root/temp/build/ksip-ui/peer-extension.txt" -Raw).Trim()

    # A second call holds the first one. Ending it hands the first call back,
    # and the window says so. Both lines behave the same way.
    foreach($pair in @((2,1),(1,2))){
        $first=$pair[0];$second=$pair[1]
        Start-Call $first $extension
        Start-Call $second $extension
        Wait-Class "line-$first" 'call-held'
        "PASS: 通話$second へ発信すると通話$first は保留になる"
        Stop-Call $second
        Wait-Class "line-$first" 'call-established'
        if(Test-Class "line-$first" 'call-held'){throw "Line $first is still on hold"}
        Wait-Class 'transfer-status' 'transfer-other-closed'
        Wait-Text 'transfer-status' '保留中の通話に戻りました' | Out-Null
        "PASS: 通話$second を切ると通話$first へ戻り、そう知らせる"
        Stop-Call $first
    }
    @{version=$version;secondCallHoldsTheFirst=$true;returnsToTheHeldCall=$true;reportsTheReturn=$true;sameOnBothLines=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-transfer-v$version.json"
    'PASS: real KSIP returns to the held call from either line, and says so'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Set-Content "$root/temp/build/ksip-ui/done" 'done'
    if($peer -and !$peer.HasExited){$peer.WaitForExit(10000) | Out-Null}
    Stop-KsipProfile
}
