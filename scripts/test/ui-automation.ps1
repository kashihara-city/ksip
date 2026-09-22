# UI Automation helpers shared by the tests that drive the real KSIP window.
# Dot-source this file, then call Set-KsipWindow with the started process.
# Controls are addressed by the HTML id, which WebView2 shows as AutomationId,
# and states are read from the HTML class, which it shows as ClassName. Neither
# changes when the wording does. Only text that a person must read is matched.
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
# A window in the tray is hidden rather than gone, so Windows is asked directly.
Add-Type @'
using System;using System.Runtime.InteropServices;
public static class KsipWindow {
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr window,int command);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr window);
}
'@
function Test-Visible($handle) { [KsipWindow]::IsWindowVisible($handle) }
function Wait-Visible($handle, [bool]$visible, [int]$seconds = 15) {
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        if ([KsipWindow]::IsWindowVisible($handle) -eq $visible) { return }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $end)
    Save-KsipElements
    throw "ウィンドウの表示が $visible になりません"
}
function Get-KsipVersion {
    # The product version lives in Cargo.toml; tests and reports follow it.
    $manifest = Get-Content -LiteralPath (Join-Path (Split-Path (Split-Path $PSScriptRoot)) 'src-tauri/Cargo.toml') -Raw
    if ($manifest -notmatch '(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"') { throw 'Cargo package version was not found' }
    $Matches[1]
}
function Get-KsipReleaseExe([string]$folder) {
    # The build puts one exe in the release folder under two names. The tests run
    # the fixed name, whose path does not change with the version, so one Windows
    # Firewall rule keeps working. The version-stamped copy is checked here too,
    # because that is the file that ships.
    $name = "ksip-v$(Get-KsipVersion).exe"
    $stamped = Join-Path $folder $name
    $exe = Join-Path $folder 'ksip.exe'
    if (!(Test-Path -LiteralPath $stamped)) { throw "Build $name first: $stamped" }
    if (!(Test-Path -LiteralPath $exe)) { throw "ksip.exe is missing from $folder" }
    if ((Get-FileHash $stamped).Hash -ne (Get-FileHash $exe).Hash) {
        throw "$name and ksip.exe are different builds"
    }
    (Resolve-Path $exe).Path
}
$script:KsipApp = $null
$script:KsipDump = $null
function Set-KsipWindow($process, [string]$dump = '') {
    $script:KsipApp = $process
    $script:KsipDump = $dump
}
function Get-KsipRoot {
    if (!$script:KsipApp) { return $null }
    $script:KsipApp.Refresh()
    if (!$script:KsipApp.MainWindowHandle) { return $null }
    # The window can go away between the check and the call, for instance while
    # another instance is still shutting down.
    try { [Windows.Automation.AutomationElement]::FromHandle($script:KsipApp.MainWindowHandle) }
    catch { $null }
}
function Find-Id([string]$id) {
    $ui = Get-KsipRoot
    if (!$ui) { return $null }
    $condition = New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::AutomationIdProperty, $id)
    $ui.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
}
function Wait-Id([string]$id, [int]$seconds = 20) {
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $node = Find-Id $id
        if ($node) { return $node }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $end)
    Save-KsipElements
    throw "UI missing id: $id"
}
function Click-Id([string]$id, [int]$seconds = 20) {
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $node = Wait-Id $id $seconds
        if ($node.Current.IsEnabled) { Press $node $id; return }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $end)
    Save-KsipElements
    throw "UI disabled: $id"
}
function Text-Id([string]$id, [int]$seconds = 20) {
    # The text of an element sits in its own name, or in the text nodes below it.
    $node = Wait-Id $id $seconds
    if ($node.Current.Name) { return $node.Current.Name }
    $condition = New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text)
    ($node.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) |
        ForEach-Object { $_.Current.Name }) -join ' '
}
function Wait-Text([string]$id, [string]$pattern, [int]$seconds = 20) {
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $text = Text-Id $id $seconds
        if ($text -match $pattern) { return $text }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $end)
    Save-KsipElements
    throw "UI text missing in ${id}: $pattern"
}
function Test-Class([string]$id, [string]$token) {
    $node = Find-Id $id
    [bool]$node -and (" $($node.Current.ClassName) " -like "* $token *")
}
function Wait-NoClass([string]$id, [string]$token, [int]$seconds = 20) {
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        if (!(Test-Class $id $token)) { return }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $end)
    Save-KsipElements
    throw "UI state stayed: $id is still $token"
}
function Wait-Class([string]$id, [string]$token, [int]$seconds = 20) {
    # A state the window shows in its class names, which no wording change moves.
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        if (Test-Class $id $token) { return }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $end)
    Save-KsipElements
    throw "UI state missing: $id is not $token"
}
function Value-Id([string]$id, [int]$seconds = 20) {
    # A field can still be disabled just after a line switch, so wait for it.
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $node = Wait-Id $id $seconds
        if ($node.Current.IsEnabled) {
            return $node.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern)
        }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $end)
    Save-KsipElements
    throw "UI disabled: $id"
}
function Select-Index([string]$id, [int]$index) {
    # Options are picked by position, because their labels are translated.
    # A select the page has only just drawn can refuse to expand for a moment,
    # or expand without its options being in the tree yet, so the whole step
    # is repeated until the option is there.
    $condition = New-Object Windows.Automation.PropertyCondition([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
    $end = [DateTime]::UtcNow.AddSeconds(10)
    while ($true) {
        $box = Wait-Id $id
        $expand = $box.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern)
        $items = $null
        try {
            $expand.Expand()
            Start-Sleep -Milliseconds 300
            $items = $box.FindAll([Windows.Automation.TreeScope]::Descendants, $condition)
        } catch { $items = $null }
        if ($items -and $index -lt $items.Count) { break }
        try { $expand.Collapse() } catch {}
        if ([DateTime]::UtcNow -ge $end) { Save-KsipElements; throw "UI missing option $index in $id" }
        Start-Sleep -Milliseconds 300
    }
    $items[$index].GetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Start-Sleep -Milliseconds 400
}
function Save-KsipElements {
    # A failing test leaves the element list behind so the cause is visible.
    if (!$script:KsipDump) { return }
    $ui = Get-KsipRoot
    if (!$ui) { return }
    $ui.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition) |
        ForEach-Object { "$($_.Current.ControlType.ProgrammaticName)`t$($_.Current.Name)" } |
        Set-Content -Encoding utf8 $script:KsipDump
}
function Press($node, [string]$name = 'element') {
    # Buttons invoke, checkboxes toggle and tabs select; the caller does not care which.
    $pattern = $null
    if ($node.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke() }
    elseif ($node.TryGetCurrentPattern([Windows.Automation.TogglePattern]::Pattern, [ref]$pattern)) { $pattern.Toggle() }
    elseif ($node.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) { $pattern.Select() }
    else { throw "No click pattern: $name" }
}
