# Check that the chosen network adapter drives the binding, and that a missing one is reported.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$key='HKCU:\Software\KashiharaCity\ksip\Test\test-ui-ksip'
$configPath=Join-Path (Get-KsipProfileFolder) 'config'
# The adapter that owns the address this machine registers from: the one
# Windows routes to the PBX through. A virtual adapter (WSL's, say) can have
# the better metric and no way to the PBX, so the metric does not decide.
$pbx=[System.Net.Dns]::GetHostAddresses((Get-KsipLab).server) | Where-Object { $_.AddressFamily -eq 'InterNetwork' } | Select-Object -First 1
if(!$pbx){throw 'The PBX address could not be resolved'}
$address=(Find-NetRoute -RemoteIPAddress $pbx.IPAddressToString | Where-Object { $_.IPAddress } | Select-Object -First 1)
if(!$address){throw 'No route to the PBX from this machine'}
$guid=(Get-NetAdapter -InterfaceIndex $address.InterfaceIndex).InterfaceGuid
function Set-Adapter([string]$value) {
    # The adapter is a registry value of its own, like every setting.
    Set-ItemProperty -LiteralPath $key -Name 'network_adapter' -Value $value -Type String
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
