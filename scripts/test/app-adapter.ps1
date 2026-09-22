# Check that the chosen network adapter drives the binding, and that a missing one is reported.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$key='HKCU:\Software\KashiharaCity\ksip\Test\test-ui-ksip'
$configPath=Join-Path $env:TEMP 'ksip-profile/test-ui-ksip/config'
# The adapter that owns the address this machine registers from.
$address=(Get-NetIPAddress -AddressFamily IPv4 -PrefixOrigin Dhcp,Manual -ErrorAction SilentlyContinue |
    Where-Object { $_.IPAddress -notlike '169.254.*' -and $_.IPAddress -ne '127.0.0.1' } |
    Sort-Object SkipAsSource,InterfaceMetric | Select-Object -First 1)
if(!$address){throw 'No usable IPv4 address on this machine'}
$guid=(Get-NetAdapter -InterfaceIndex $address.InterfaceIndex).InterfaceGuid
function Set-Adapter([string]$value) {
    # The adapter lives in the settings document, like the other client choices.
    $settings = (Get-ItemProperty -LiteralPath $key).Settings | ConvertFrom-Json
    $settings | Add-Member -NotePropertyName 'network_adapter' -NotePropertyValue $value -Force
    Set-ItemProperty -LiteralPath $key -Name 'Settings' -Value ($settings | ConvertTo-Json -Compress)
}
$app=$null
try {
    Start-KsipProfile
    Set-Adapter $guid
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    $config=Get-Content $configPath -Raw
    foreach($line in "sip_listen $($address.IPAddress):","net_interface $guid"){
        if($config -notmatch [regex]::Escape($line)){throw "Engine config is missing: $line"}
    }
    "PASS: 選んだアダプターのアドレスで待ち受け、エンジンにもアダプターを渡す"
    Stop-KsipApp $app; $app=$null
    Stop-KsipEngine
    # An adapter that is not there must be named, not silently ignored.
    Set-Adapter '{00000000-0000-0000-0000-000000000000}'
    $app=Start-KsipApp "$Folder/ksip.exe" "$root/temp/build/ui-failure.txt"
    Wait-Text 'error' '指定NICが見つかりませんでした' | Out-Null
    "PASS: 存在しないアダプターはそのまま伝える"
    @{version=$version;adapter=$guid;boundAddress=$address.IPAddress;reportsMissingAdapter=$true} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-adapter-v$version.json"
    'PASS: real KSIP binds the chosen network adapter'
} finally {
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
