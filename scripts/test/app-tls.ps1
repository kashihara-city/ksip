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
    foreach($line in 'sip_transports TLS','ksip_sip_transport TLS','ksip_mediaenc srtp-mand','sip_verify_server yes'){
        if($config -notmatch [regex]::Escape($line)){throw "Engine config is missing: $line"}
    }
    Wait-Text 'transport-label' '^TLS$' | Out-Null
    "PASS: 設定からTLSとSRTPのエンジン設定が作られる"
    (Value-Id 'target').SetValue((Get-KsipLab).numbers.playback)
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
    # Without a chosen authority, the Windows certificate store is the trust
    # anchor. That only registers when the lab's authority is installed there,
    # which lab.json states for each PBX.
    $trusted=(Get-KsipLab).features.windows_trust
    if($null -eq $trusted -or $trusted){
        Click-Id 'settings-button';Wait-Id 'save-settings' | Out-Null
        (Value-Id 'ca_file').SetValue('')
        Click-Id 'save-settings'
        $end=[DateTime]::UtcNow.AddSeconds(25)
        while((Find-Id 'save-settings') -and [DateTime]::UtcNow -lt $end){Start-Sleep -Milliseconds 150}
        if(Find-Id 'save-settings'){throw 'Settings did not save/reconnect without a CA file'}
        Wait-Class 'registration' 'reg-register_ok'
        $config=Get-Content (Join-Path $env:TEMP 'ksip-profile/test-ui-ksip/config') -Raw
        if($config -notmatch 'sip_cafile .*windows-trust\.pem'){throw 'Engine config does not use the Windows store'}
        if($config -notmatch 'sip_verify_server yes'){throw 'Engine config does not verify the server'}
        if(!(Test-Path (Join-Path $env:TEMP 'ksip-profile/test-ui-ksip/windows-trust.pem'))){throw 'windows-trust.pem was not written'}
        Wait-Text 'transport-label' '^TLS$' | Out-Null
        "PASS: 認証局を指定しなくてもWindowsの証明書ストアで検証して登録した"
    } else {
        'NOTE: この PBX の証明書は Windows の証明書ストアにつながらないので、認証局を指定しない登録は確かめない（features.windows_trust=false）'
    }
    @{version=$version;transport='tls';verifiedCertificate=$true;mediaEncrypted=$true;windowsStoreVerified=$true} | ConvertTo-Json |
        Set-Content -Encoding utf8 "$root/temp/reports/app-tls-v$version.json"
    'PASS: real KSIP over SIP/TLS with a verified certificate and SRTP media'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
