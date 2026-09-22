# Check the app itself over SIP/TLS with a verified certificate and SRTP media.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
Start-KsipProfile 'tls'
$app=$null
try {
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    # The engine configuration is built from the saved settings, not hard coded.
    $config=Get-Content (Join-Path $env:TEMP 'ksip-profile/test-ui-ksip/config') -Raw
    foreach($line in 'sip_transports TLS','ksip_sip_transport TLS','ksip_mediaenc srtp','sip_verify_server yes'){
        if($config -notmatch [regex]::Escape($line)){throw "Engine config is missing: $line"}
    }
    Wait-Text 'transport-label' '^TLS$' | Out-Null
    "PASS: 設定からTLSとSRTPのエンジン設定が作られる"
    (Value-Id 'target').SetValue('9001')
    Click-Id 'dial'
    Wait-Class 'line-1' 'call-established'
    Start-Sleep -Seconds 4
    $rows=Get-KsipLog
    $tls=@($rows | Where-Object { $_.text -match 'transport=tls|/TLS/' }).Count
    $srtp=@($rows | Where-Object { $_.text -match 'SRTP is Enabled' })
    if($tls -eq 0){throw 'No TLS signalling in the log'}
    if(!$srtp){throw 'Media was not encrypted'}
    # The footer names the negotiated encryption, not the setting.
    Wait-Text 'codec-label' 'SRTP（SDES）' | Out-Null
    ($srtp | Select-Object -Last 1).text
    "PASS: 実アプリがTLSで登録しSRTPで通話した"
    Click-Id 'hangup'
    Wait-Class 'line-1' 'call-idle'
    @{version=$version;transport='tls';verifiedCertificate=$true;mediaEncrypted=$true} | ConvertTo-Json |
        Set-Content -Encoding utf8 "$root/temp/reports/app-tls-v$version.json"
    'PASS: real KSIP over SIP/TLS with a verified certificate and SRTP media'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
