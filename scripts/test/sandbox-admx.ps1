# Runs inside Windows Sandbox for sandbox-admx.py: places the KSIP templates, drives gpedit through UIAutomation, and records what each policy change writes.
# Heavy and local only: started by sandbox-admx.py, never on its own and never in CI.
param([string]$Plan, [string]$Out)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
$AE = [System.Windows.Automation.AutomationElement]
$Scope = [System.Windows.Automation.TreeScope]
$CT = [System.Windows.Automation.ControlType]
# Not $plan: PowerShell names are case-blind, and the [string] parameter
# $Plan would turn the parsed plan back into text.
$spec = Get-Content -Raw -Encoding UTF8 $Plan | ConvertFrom-Json
$result = [ordered]@{ language = ''; load_errors = @(); listing = @(); steps = @(); fatal = '' }
$log = Join-Path $Out 'inside.log'
function Say([string]$text) { Add-Content -Encoding UTF8 $log ("{0:HH:mm:ss} {1}" -f (Get-Date), $text) }
function Save-Result { $result | ConvertTo-Json -Depth 8 | Set-Content -Encoding UTF8 (Join-Path $Out 'result.json') }

function Wait-For([scriptblock]$find, [int]$seconds = 20, [string]$what = 'element') {
    $end = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $found = & $find
        if ($found) { return $found }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $end)
    throw "not found: $what"
}
function Cond($property, $value) { New-Object System.Windows.Automation.PropertyCondition($property, $value) }
function By-Name($parent, [string]$name, $type = $null, $scope = $Scope::Descendants) {
    $cond = Cond $AE::NameProperty $name
    if ($type) { $cond = New-Object System.Windows.Automation.AndCondition($cond, (Cond $AE::ControlTypeProperty $type)) }
    $parent.FindFirst($scope, $cond)
}
function Of-Type($parent, $type) { $parent.FindAll($Scope::Descendants, (Cond $AE::ControlTypeProperty $type)) }
function Top-Window([string]$name) { By-Name $AE::RootElement $name $CT::Window $Scope::Children }
function Dump($element, [string]$file) {
    try {
        $element.FindAll($Scope::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
            ForEach-Object { "$($_.Current.ControlType.ProgrammaticName)`t$($_.Current.Name)`t$($_.Current.AutomationId)" } |
            Set-Content -Encoding UTF8 (Join-Path $Out $file)
    } catch {}
}
function Select-Item($item) { $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select() }
function Invoke($element) { $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
# A click in the middle of an element, for the controls of gpedit's policy
# dialog, which show UIAutomation no pattern to act through.
Add-Type -Namespace KsipTest -Name Mouse -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, System.UIntPtr extra);
'@
function Click($element) {
    $r = $element.Current.BoundingRectangle
    [KsipTest.Mouse]::SetCursorPos([int]($r.X + $r.Width / 2), [int]($r.Y + $r.Height / 2)) | Out-Null
    Start-Sleep -Milliseconds 100
    [KsipTest.Mouse]::mouse_event(0x2, 0, 0, 0, [UIntPtr]::Zero)
    [KsipTest.Mouse]::mouse_event(0x4, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 300
}
function Focused { try { $AE::FocusedElement } catch { $null } }
function Inside($element, $area) {
    $r = $element.Current.BoundingRectangle
    $x = $r.X + $r.Width / 2; $y = $r.Y + $r.Height / 2
    ($x -ge $area.X) -and ($x -le $area.X + $area.Width) -and ($y -ge $area.Y) -and ($y -le $area.Y + $area.Height)
}
# Text as SendKeys takes it: its own marks are escaped.
function Keys([string]$text) { ($text.ToCharArray() | ForEach-Object { if ('+^%~(){}[]'.Contains([string]$_)) { '{' + $_ + '}' } else { [string]$_ } }) -join '' }
# The policy dialog belongs to gpedit's window, not to the desktop.
function Find-Dialog([string]$name) {
    $d = $null
    if ($script:main) { $d = By-Name $script:main $name $CT::Window }
    if (!$d) { $d = Top-Window $name }
    $d
}
function List-Names($list) { @($list.FindAll($Scope::Children, (Cond $AE::ControlTypeProperty $CT::ListItem)) | ForEach-Object { $_.Current.Name }) }

# gpedit, started afresh at its root for every look: MMC's scope tree does
# not show its items to UIAutomation, so the way in is the results list,
# item by item, as a double click would go.
$script:mmc = $null
$script:main = $null
$script:first = $true
function Start-Gpedit {
    if ($script:mmc) {
        Stop-Process -Id $script:mmc.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 800
    }
    $script:mmc = Start-Process mmc.exe -ArgumentList "$env:WINDIR\System32\gpedit.msc" -PassThru
    $script:main = $null
    $end = [DateTime]::UtcNow.AddSeconds(60)
    while (!$script:main -and [DateTime]::UtcNow -lt $end) {
        foreach ($w in $AE::RootElement.FindAll($Scope::Children, [System.Windows.Automation.Condition]::TrueCondition)) {
            if ($w.Current.ProcessId -ne $script:mmc.Id) { continue }
            $list = $w.FindFirst($Scope::Descendants, (Cond $AE::ControlTypeProperty $CT::List))
            if ($list) {
                if (@(List-Names $list).Count -gt 0) { $script:main = $w }
                continue
            }
            # A window of mmc without the results list is a message: kept
            # (the first start is where the templates are read), then dismissed.
            $words = (Of-Type $w $CT::Text | ForEach-Object { $_.Current.Name }) -join ' '
            if ($words -and $script:first) {
                $script:result.load_errors += "$($w.Current.Name): $words"
                Say "dialog: $($w.Current.Name): $words"
            }
            $ok = By-Name $w 'OK' $CT::Button
            if ($ok) { Invoke $ok }
        }
        Start-Sleep -Milliseconds 400
    }
    $script:first = $false
    if (!$script:main) { throw 'the gpedit window did not appear' }
}
function Results { $script:main.FindFirst($Scope::Descendants, (Cond $AE::ControlTypeProperty $CT::List)) }
function Names { $l = Results; if ($l) { List-Names $l } else { @() } }
# Selects an item of the results list and opens it (Enter), as a double click would.
function Open-Item([string]$name) {
    $item = Wait-For { $l = Results; if ($l) { By-Name $l $name $CT::ListItem } } 15 "list item $name"
    Select-Item $item
    $item.SetFocus()
    Start-Sleep -Milliseconds 200
    [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
}
# From the root, folder by folder: each is opened and its contents awaited.
# The templates folder can take a while the first times it is read, so a
# way in that times out is tried once more from the start.
function Go([string[]]$names) {
    for ($try = 1; ; $try++) {
        try {
            Start-Gpedit
            foreach ($n in $names) {
                Open-Item $n
                Wait-For { $now = @(Names); ($now.Count -gt 0) -and ($now -notcontains $n) } 20 "the contents of $n" | Out-Null
                Start-Sleep -Milliseconds 400
            }
            return
        } catch {
            if ($try -ge 2) { throw }
            Say "retrying the way to $($names[-1]): $_"
        }
    }
}

try {
    Say "powershell $($PSVersionTable.PSVersion)"
    Say 'placing the templates'
    Copy-Item "$PSScriptRoot\admx\ksip.admx" "$env:WINDIR\PolicyDefinitions\" -Force
    foreach ($l in 'ja-JP', 'en-US') {
        New-Item -ItemType Directory -Force "$env:WINDIR\PolicyDefinitions\$l" | Out-Null
        Copy-Item "$PSScriptRoot\admx\$l\ksip.adml" "$env:WINDIR\PolicyDefinitions\$l\" -Force
    }
    Say 'starting gpedit'
    Start-Gpedit
    # Which language gpedit speaks: its root lists the two configurations.
    $top = @(Names)
    foreach ($l in 'ja-JP', 'en-US') {
        if ($top -contains @($spec.tree.$l)[1]) { $result.language = $l; break }
    }
    if (!$result.language) { Dump $script:main 'dump-main.txt'; throw "the root was not recognised: $($top -join ', ')" }
    $lang = $result.language
    Say "language $lang"
    $base = @(@($spec.tree.$lang)[1..2])

    # What gpedit lists in each KSIP category.
    foreach ($entry in $spec.listing) {
        $names = $base + @($entry.path | ForEach-Object { $_.$lang })
        $expected = @($entry.policies | ForEach-Object { $_.$lang })
        try {
            Go $names
            Wait-For { (Names) -contains $expected[0] } 10 "policies of $($names[-1])" | Out-Null
            Start-Sleep -Milliseconds 500
            $items = @(Names)
            $result.listing += [ordered]@{ path = $names[-1]; items = $items }
            Say "listed $($names[-1]): $($items.Count)"
        } catch {
            $result.listing += [ordered]@{ path = $names[-1]; items = @(); error = "$_" }
            Say "listing $($names[-1]) failed: $_"
            Dump $script:main "dump-listing-$($result.listing.Count).txt"
        }
    }
    Save-Result

    # The steps: open the policy, set its state and values, OK, apply, read the key.
    $i = 0
    foreach ($step in $spec.steps) {
        $i++
        $entry = [ordered]@{ policy = $step.policy; state = $step.state; registry = @{}; error = '' }
        $name = $step.name.$lang
        try {
            Go ($base + @($step.path | ForEach-Object { $_.$lang }))
            Open-Item $name
            $dialog = Wait-For { Find-Dialog $name } 15 "dialog $name"
            Start-Sleep -Milliseconds 800
            # The state buttons are named in English whatever the language.
            $word = $spec.states.'en-US'.($step.state)
            $radio = Wait-For { By-Name $dialog $word } 10 "state $word"
            Click $radio
            Start-Sleep -Milliseconds 600
            Dump $dialog "dump-step$i-after-state.txt"
            if (@($step.set).Count -gt 0) {
                # The fields are not in the UIAutomation tree, but the focus is:
                # Tab moves from the state buttons past the comment to the
                # "supported on" box, and the next Tab is the first field; then
                # field by field in presentation order.
                $found = $false
                for ($t = 1; $t -le 10 -and !$found; $t++) {
                    [System.Windows.Forms.SendKeys]::SendWait('{TAB}')
                    Start-Sleep -Milliseconds 150
                    $f = Focused
                    Say ("  tab {0}: {1}" -f $t, $(if ($f) { $f.Current.Name } else { 'none' }))
                    if ($f -and $f.Current.Name -eq $spec.supported.$lang) { $found = $true }
                }
                if (!$found) { throw 'the focus never reached the supported-on box' }
                [System.Windows.Forms.SendKeys]::SendWait('{TAB}')
                Start-Sleep -Milliseconds 150
                $position = 0
                foreach ($set in $step.set) {
                    while ($position -lt $set.order) { [System.Windows.Forms.SendKeys]::SendWait('{TAB}'); Start-Sleep -Milliseconds 150; $position++ }
                    if ($set.kind -eq 'enum') {
                        [System.Windows.Forms.SendKeys]::SendWait('{HOME}')
                        for ($d = 0; $d -lt $set.index; $d++) { [System.Windows.Forms.SendKeys]::SendWait('{DOWN}'); Start-Sleep -Milliseconds 80 }
                    } else {
                        [System.Windows.Forms.SendKeys]::SendWait('{HOME}+{END}{DEL}')
                        [System.Windows.Forms.SendKeys]::SendWait((Keys $set.value.$lang))
                    }
                    Start-Sleep -Milliseconds 200
                    $f = Focused
                    Say ("  set {0} ({1}): {2} [{3}]" -f $set.order, $set.kind, $(if ($f) { $f.Current.ControlType.ProgrammaticName } else { 'none' }), $(if ($f) { $f.Current.Name } else { '' }))
                }
            }
            Click (By-Name $dialog 'OK')
            Wait-For { !(Find-Dialog $name) } 10 'the dialog to close' | Out-Null
            & gpupdate.exe /target:user /force /wait:120 2>&1 | Out-Null
            $key = 'HKCU:\Software\Policies\KashiharaCity\ksip'
            if (Test-Path $key) {
                $k = Get-Item $key
                foreach ($n in $k.GetValueNames()) { $entry.registry[$n] = @{ kind = "$($k.GetValueKind($n))"; data = "$($k.GetValue($n))" } }
            }
            Say "step $i $($step.policy) $($step.state): $(@($entry.registry.Keys) -join ',')"
        } catch {
            $entry.error = "$_ at $($_.InvocationInfo.ScriptLineNumber)"
            Say "step $i failed: $($entry.error)"
            $dialog = Find-Dialog $name
            if ($dialog) {
                Dump $dialog "dump-step$i.txt"
                $cancel = By-Name $dialog 'Cancel'
                if ($cancel) { Click $cancel }
            } elseif ($script:main) {
                Dump $script:main "dump-step$i.txt"
            }
        }
        $result.steps += $entry
        Save-Result
    }
} catch {
    $result.fatal = "$_ at $($_.InvocationInfo.ScriptLineNumber): $($_.InvocationInfo.Line.Trim())"
    Say "fatal: $($result.fatal)"
    Say $_.ScriptStackTrace
}
if ($script:mmc) { Stop-Process -Id $script:mmc.Id -Force -ErrorAction SilentlyContinue }
Save-Result
Say 'done'
# The host waits for this, not for the result, which is also saved on the way.
Set-Content -Encoding ASCII (Join-Path $Out 'done.txt') 'done'
if (!$spec.keep) { Stop-Computer -Force }
