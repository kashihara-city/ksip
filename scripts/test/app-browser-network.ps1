# Check that the window's browser (WebView2) is started quiet behind the refusing proxy and that the app's own check says so in the log; that the footer says so when the proxy is not in the way; and that the app comes up again right after a quit, on another port.
param([string]$Folder = "$PSScriptRoot/../../temp/build/gui-ksip")
$ErrorActionPreference='Stop'
. "$PSScriptRoot/app-fixture.ps1"
$root=Split-Path (Split-Path $PSScriptRoot)
Use-KsipBuild $Folder $PSBoundParameters.ContainsKey('Folder')
$version=Get-KsipVersion
New-Item -ItemType Directory -Force "$root/temp/reports" | Out-Null
$exe="$Folder/ksip.exe"
function Get-BrowserArguments {
    # The browser process of the test profile's window (the one that is no
    # helper of another), as it was started.
    $end=[DateTime]::UtcNow.AddSeconds(15)
    do {
        $browser=Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" |
            Where-Object { $_.CommandLine -like '*test-ui-ksip*' -and $_.CommandLine -notmatch '--type=' } | Select-Object -First 1
        if($browser){return $browser.CommandLine}
        Start-Sleep -Milliseconds 200
    } while([DateTime]::UtcNow -lt $end)
    throw 'The browser process of the window was not found'
}
function Get-ProxyPort([string]$arguments) {
    if($arguments -notmatch '--proxy-server=127\.0\.0\.1:(\d+)'){throw "The browser was started without the refusing proxy: $arguments"}
    [int]$Matches[1]
}
# The lines of this run that say what the check found, oldest first.
function Get-Findings { @(Get-KsipLog | Where-Object { $_.src -eq 'app' -and $_.text -match "window's browser: (its own requests are refused|NOT kept from the network)" }) }
function Wait-Finding([int]$count, [string]$pattern) {
    $end=[DateTime]::UtcNow.AddSeconds(20)
    do {
        # As an array whatever their number: one row alone has no count here.
        $found=@(Get-Findings)
        if($found.Count -ge $count){
            if($found[$count-1].text -notmatch $pattern){throw "The check said: $($found[$count-1].text)"}
            return
        }
        Start-Sleep -Milliseconds 200
    } while([DateTime]::UtcNow -lt $end)
    throw "The check's finding number $count is not in the log"
}
Start-KsipProfile
$app=$null
try {
    $app=Start-KsipApp $exe "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    $arguments=Get-BrowserArguments
    $first=Get-ProxyPort $arguments
    foreach($wanted in '--disable-background-networking','--disable-component-update','--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection'){
        if($arguments -notmatch [regex]::Escape($wanted)){throw "The browser was started without $wanted"}
    }
    Wait-Finding 1 "its own requests are refused .*127\.0\.0\.1:$first"
    if(Find-Id 'browser-open'){throw 'The footer says the browser is not kept from the network, though the check found it kept'}
    'PASS: 窓のブラウザーは断るプロキシを通して静かに起動し、アプリの確認がログに「止めている」と書く'

    # With the proxy argument overridden from outside (as a browser that no
    # longer takes it would behave), the page's probe does not come to the
    # proxy: the footer says so in a few words, the log says so, and the phone
    # works as before. Started the moment the first has quit: a browser with
    # other arguments cannot be made while the old one still holds its data.
    Stop-KsipApp $app; Stop-KsipEngine
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--no-proxy-server'
    try { $app=Start-KsipApp $exe "$root/temp/build/ui-failure.txt" } finally { $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=$null }
    Wait-Class 'registration' 'reg-register_ok'
    # The note's name, as a screen reader says it, is its explanation (the
    # tooltip); the few words shown are the text below it.
    Wait-Text 'browser-open' '止められていません。電話の機能には影響しません' | Out-Null
    $words=New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text)
    $shown=((Wait-Id 'browser-open').FindAll([Windows.Automation.TreeScope]::Descendants, $words) | ForEach-Object { $_.Current.Name }) -join ' '
    if($shown -ne 'WebView2通信遮断: 無効'){throw "The footer shows: $shown"}
    Wait-Finding 2 'NOT kept from the network'
    'PASS: プロキシが効いていないと、フッターに小さく出し、ログに書く。電話は登録できる'

    Stop-KsipApp $app; Stop-KsipEngine
    $app=Start-KsipApp $exe "$root/temp/build/ui-failure.txt"
    Wait-Class 'registration' 'reg-register_ok'
    $second=Get-ProxyPort (Get-BrowserArguments)
    Wait-Finding 3 "its own requests are refused .*127\.0\.0\.1:$second"
    if(Find-Id 'browser-open'){throw 'The footer still says the browser is not kept from the network'}
    'PASS: 終了の直後に起動し直しても窓が出て、新しいプロキシで止めている'
    @{version=$version;quietBehindProxy=$true;checkLogged=$true;saysWhenNotKept=$true;restartsAtOnce=$true;ports=@($first,$second)} |
        ConvertTo-Json | Set-Content -Encoding utf8 "$root/temp/reports/app-browser-network-v$version.json"
    "PASS: the window's browser is kept from the network, and the app says when it is not"
} finally {
    $env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=$null
    Stop-KsipApp $app
    Stop-KsipEngine
    Stop-KsipProfile
}
