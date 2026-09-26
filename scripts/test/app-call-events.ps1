# Check that calls which come and go between two looks at the engine still reach the history: an incoming call cancelled within the poll interval is a missed call, and a call refused under do not disturb is kept even when a state query lands right after it.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
# The buttons profile has the do-not-disturb button (custom-8).
Start-KsipProfile 'buttons'
$app=$null
function Count-History([string]$word){
    $text=(Text-Id 'call-history')
    ([regex]::Matches($text,[regex]::Escape($word))).Count
}
function Start-QuickCalls([double]$hold, [int]$times){
    # The fixture dials this phone from the second account and hangs up after $hold seconds, $times times.
    Start-Process python -ArgumentList @('-X','utf8',"$PSScriptRoot/app_fixture.py",'quick',$hold,$times) -PassThru -WindowStyle Hidden
}
try {
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    Click-Id 'history-tab'

    # Cancelled 100 ms after the caller dialled, inside the 300 ms between two
    # polls: such a call is mostly never in a snapshot, so its row has to come
    # from the events. A PBX that routes as a B2BUA may swallow a call that is
    # cancelled before it reaches this phone; what counts is that every call
    # the phone was told about (CALL_INCOMING) has its row.
    # A B2BUA may also keep ringing this phone for a while after the caller
    # gave up (3CX does, until its own ring timeout), so what is counted is the
    # calls that both arrived and ended here (CALL_INCOMING with CALL_CLOSED).
    $missedBefore=Count-History '不在着信'
    # ($args inside Where-Object would be that block's own, so the prefix is a parameter.)
    $events={ param($prefix) @(Get-KsipLog | Where-Object { $_.src -eq 'event' -and $_.text -like "$prefix*" }).Count }
    $incomingBefore=& $events 'CALL_INCOMING';$closedBefore=& $events 'CALL_CLOSED'
    $quick=Start-QuickCalls 0.1 3
    if(!$quick.WaitForExit(60000)){Stop-Process -Id $quick.Id -Force;throw 'the quick caller did not finish'}
    if($quick.ExitCode -ne 0){throw "the quick caller failed ($($quick.ExitCode))"}
    Start-Sleep -Seconds 2
    $arrived=(& $events 'CALL_INCOMING')-$incomingBefore
    $ended=[Math]::Min($arrived,(& $events 'CALL_CLOSED')-$closedBefore)
    if($arrived -lt 1){throw 'none of the three quick calls reached this phone; the PBX swallowed them all'}
    if($ended -lt 1){throw "$arrived quick calls reached this phone but none has ended yet"}
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Count-History '不在着信') -lt $missedBefore+$ended -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    $missedAfter=Count-History '不在着信'
    if($missedAfter -lt $missedBefore+$ended){throw "missed-call rows went $missedBefore -> $missedAfter although $ended quick calls came and went"}
    "PASS: ポーリングの間に終わった着信も不在着信として履歴に残る（届いて終わった $ended 件すべて）"
    if($ended -lt $arrived){"NOTE: $($arrived-$ended) 件は相手が切った後も PBX がこの電話を鳴らし続けている（3CX の振る舞い）"}
    # Whatever the PBX still rings ends by its own ring timeout before the next stage.
    $end=[DateTime]::UtcNow.AddSeconds(45)
    while(((Test-Class 'line-1' 'call-incoming') -or (Test-Class 'line-2' 'call-incoming')) -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 500}

    # Refused under do not disturb, each followed at once by a device refresh,
    # which asks the engine for its state outside the polling. The refusal
    # must not be consumed by that query.
    Click-Id 'custom-8';Wait-Class 'custom-8' 'dnd'
    $refusedBefore=Count-History '着信拒否'
    for($i=0;$i -lt 3;$i++){
        $one=Start-QuickCalls 0.3 1
        Start-Sleep -Milliseconds 700
        Click-Id 'refresh-devices'
        if(!$one.WaitForExit(30000)){Stop-Process -Id $one.Id -Force;throw 'the refused caller did not finish'}
    }
    $end=[DateTime]::UtcNow.AddSeconds(5)
    while((Count-History '着信拒否') -lt $refusedBefore+3 -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 200}
    $refusedAfter=Count-History '着信拒否'
    if($refusedAfter -lt $refusedBefore+3){throw "refused rows went $refusedBefore -> $refusedAfter for three refused calls"}
    'PASS: 拒否着信は状態照会に消費されず全部履歴に残る'
    Click-Id 'custom-8';Wait-NoClass 'custom-8' 'dnd'

    @{version=$version;missedWithinPoll=$true;refusedNotConsumed=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-call-events-v$version.json"
    'PASS: real KSIP keeps calls that came and went between polls in the history'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
