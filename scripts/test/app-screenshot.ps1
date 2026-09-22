# Capture the window of the running app; used by the app tests.
param([switch]$CaptureOnly)
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class CapturePostSip {
 [StructLayout(LayoutKind.Sequential)] public struct Rect {public int Left,Top,Right,Bottom;}
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out Rect rect);
 [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h,int cmd);
}
'@
$app=Get-Process -Id ([int](Get-Content "$PSScriptRoot/../../temp/build/gui-test-pid.txt"))
[CapturePostSip]::ShowWindow($app.MainWindowHandle,4) | Out-Null
Start-Sleep -Seconds 2
$rect=New-Object CapturePostSip+Rect
[CapturePostSip]::GetWindowRect($app.MainWindowHandle,[ref]$rect) | Out-Null
$bitmap=New-Object Drawing.Bitmap(($rect.Right-$rect.Left),($rect.Bottom-$rect.Top))
$graphics=[Drawing.Graphics]::FromImage($bitmap)
$dc=$graphics.GetHdc()
try {[CapturePostSip]::PrintWindow($app.MainWindowHandle,$dc,2) | Out-Null} finally {$graphics.ReleaseHdc($dc)}
$bitmap.Save((Join-Path (Split-Path (Split-Path $PSScriptRoot)) 'temp/build/gui-smoke.png'))
$graphics.Dispose();$bitmap.Dispose()
if (!$CaptureOnly) {
 $ui=[Windows.Automation.AutomationElement]::FromHandle($app.MainWindowHandle)
 $nodes=$ui.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)
 $nodes | ForEach-Object {"$($_.Current.ControlType.ProgrammaticName) $($_.Current.Name)"} | Select-Object -First 100
}
