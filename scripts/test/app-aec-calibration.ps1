# Check the AEC delay calibration controls in the settings dialog.
param([string]$Folder = "$PSScriptRoot/../../release")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
Start-KsipProfile
$app=$null
function Delay-Edit { Value-Id 'aec_delay_ms' }
function Wait-Recommendation {
    $end=[DateTime]::UtcNow.AddSeconds(15)
    do {
        $ui=Get-KsipRoot
        if($ui){
            $nodes=$ui.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)
            foreach($node in $nodes){
                if($node.Current.Name -match '^(推奨|参考（不安定）) (\d+)ms（(\d+)信号・ばらつき (\d+)ms・信頼度 (\d+)%）$'){return @([int]$Matches[2],[int]$Matches[3],[int]$Matches[4],[int]$Matches[5],($Matches[1] -eq '推奨'))}
            }
        }
        Start-Sleep -Milliseconds 150
    } while([DateTime]::UtcNow -lt $end)
    $ui=Get-KsipRoot
    $visible=@()
    if($ui){$visible=@($ui.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)|ForEach-Object{$_.Current.Name}|Where-Object{$_})}
    $visible|Set-Content -Encoding utf8 "$root/temp/build/aec-calibration-timeout.txt"
    throw "Calibration recommendation was not shown: $($visible -join ' | ')"
}
try {
    $app=Start-KsipApp (Get-KsipReleaseExe $Folder) "$root/temp/build/aec-calibration-ui-elements.txt"
    Wait-Class 'registration' 'reg-register_ok'
    Click-Id 'settings-button';Wait-Id 'save-settings' | Out-Null
    $delay=Delay-Edit
    if([int]$delay.Current.Value -ne 20){throw "Unexpected initial delay: $($delay.Current.Value)"}
    Click-Id 'calibrate-aec'
    $result=Wait-Recommendation
    if([int]$delay.Current.Value -ne $result[0]){throw 'Recommendation was not copied to the delay field'}
    Click-Id 'close-settings';Click-Id 'settings-button'
    if([int](Delay-Edit).Current.Value -ne 20){throw 'Closing settings persisted the recommendation'}
    Click-Id 'calibrate-aec-careful'
    $careful=Wait-Recommendation
    if($careful[1] -lt 2){throw "Careful calibration used only $($careful[1]) probes"}
    if([int](Delay-Edit).Current.Value -ne $careful[0]){throw 'Careful recommendation was not copied to the delay field'}
    (Delay-Edit).SetValue('24')
    Click-Id 'save-settings';Wait-Class 'registration' 'reg-register_ok'
    $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\KashiharaCity\ksip\Test\test-ui-ksip')
    try{$saved=($key.GetValue('Settings')|ConvertFrom-Json).aec_delay_ms}finally{$key.Dispose()}
    if($saved -ne 24){throw "Saved delay is $saved instead of 24"}
    # The engine keeps its profile under the OS temp folder.
    $config=Get-Content (Join-Path $env:TEMP 'ksip-profile/test-ui-ksip/config') -Raw
    if($config -notmatch '(?m)^webrtc_aec_delay_ms 24$'){throw 'Generated engine config is missing the saved delay'}
    @{version=$version;recommendationInserted=$true;recommendedMs=$result[0];simpleProbes=$result[1];simpleSpreadMs=$result[2];simpleConfidence=$result[3];simpleStable=$result[4];carefulRecommendedMs=$careful[0];carefulProbes=$careful[1];carefulSpreadMs=$careful[2];carefulConfidence=$careful[3];carefulStable=$careful[4];closeDiscards=$true;savePersists=$true;engineConfigApplied=$true} | ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/ui-aec-calibration-v$version.json"
    "PASS: simple $($result[0]) ms/$($result[1]) probes, careful $($careful[0]) ms/$($careful[1]) probes, discard/save semantics"
} finally {
    if($app){$app.Refresh();if(!$app.HasExited){$node=Find-Id 'quit';if($node){$node.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()};$app.WaitForExit(10000)|Out-Null}}
    if($app -and !$app.HasExited){Stop-KsipEngine;Stop-Process -Id $app.Id -Force}
    Stop-KsipProfile
}
