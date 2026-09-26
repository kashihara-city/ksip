# Disposable profile and window lifecycle shared by the tests that drive the app.
# Dot-source this file; it brings the UI Automation helpers with it.
. "$PSScriptRoot/ui-automation.ps1"
$script:KsipFixture = "$PSScriptRoot/app_fixture.py"
$script:KsipOldProfile = $null
function Start-KsipProfile([string]$mode = '') {
    # Every app test runs against a throw-away registry profile and credential.
    $script:KsipOldProfile = $env:KSIP_TEST_PROFILE
    $env:KSIP_TEST_PROFILE = 'test-ui-ksip'
    # The log file outlives a run, so only lines from this one are looked at.
    $script:KsipLogSince = [DateTimeOffset]::Now.AddSeconds(-1)
    python -X utf8 $script:KsipFixture setup $mode
    if ($LASTEXITCODE -ne 0) { throw 'Test profile setup failed' }
}
$script:KsipLab = $null
function Get-KsipLab {
    # The PBX the tests run against and its dialplan conventions (playback,
    # park slots, group, voicemail, an unassigned number), from lab.json.
    if (!$script:KsipLab) {
        $json = python -X utf8 $script:KsipFixture lab
        if ($LASTEXITCODE -ne 0) { throw 'lab.json could not be read' }
        $script:KsipLab = $json | ConvertFrom-Json
    }
    $script:KsipLab
}
function Stop-KsipProfile {
    python -X utf8 $script:KsipFixture cleanup
    $env:KSIP_TEST_PROFILE = $script:KsipOldProfile
}
function Stop-KsipEngine {
    python -X utf8 $script:KsipFixture stop-engine
}
function Start-KsipFixturePeer([string]$mode, [string]$logDirectory) {
    New-Item -ItemType Directory -Force $logDirectory | Out-Null
    Start-Process python -ArgumentList '-X', 'utf8', $script:KsipFixture, $mode -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput "$logDirectory/$mode-output.log" -RedirectStandardError "$logDirectory/$mode-error.log"
}
function Get-KsipLog {
    # The app appends its log one JSON object per line: time, source and text.
    $file = "$PSScriptRoot/../../temp/build/test-ui-ksip/ksip-log.jsonl"
    if (!(Test-Path $file)) { return @() }
    $since = if ($script:KsipLogSince) { $script:KsipLogSince } else { [DateTimeOffset]::MinValue }
    @(Get-Content $file -Encoding UTF8 | Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json } |
        Where-Object { [DateTimeOffset]::Parse($_.time) -ge $since })
}
function Wait-KsipLog([string]$source, [string]$pattern, [int]$seconds = 15) {
    # The file is written at most once a second, so a line takes a moment to appear.
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        # An app line carries the name of what happened; an engine line its own words.
        $row = @(Get-KsipLog | Where-Object { $_.src -eq $source -and ($_.text -match $pattern -or $_.code -match $pattern) })
        if ($row) { return $row[0] }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $end)
    throw "ログに出ません: [$source] $pattern"
}
function Use-KsipBuild([string]$folder, [bool]$staged) {
    # GUI tests run the freshly built exe unless the caller points at another folder.
    if ($staged) { return }
    $source = Join-Path (Split-Path (Split-Path $PSScriptRoot)) 'release/ksip.exe'
    if (!(Test-Path $source)) { throw "release/ksip.exe がありません。先にビルドしてください" }
    New-Item -ItemType Directory -Force $folder | Out-Null
    Copy-Item $source (Join-Path $folder 'ksip.exe') -Force
}
function Start-KsipApp([string]$path, [string]$elementDump = '') {
    $app = Start-Process $path -WindowStyle Hidden -PassThru
    Set-KsipWindow $app $elementDump
    $app
}
function Stop-KsipApp($app) {
    if (!$app) { return }
    $app.Refresh()
    if (!$app.HasExited) {
        $node = Find-Id 'quit'
        if ($node) { Press $node 'quit' }
        $app.WaitForExit(10000) | Out-Null
    }
    if (!$app.HasExited) {
        Stop-KsipEngine
        Stop-Process -Id $app.Id -Force
    }
}
