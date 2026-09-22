# MSVC toolchain, cargo target and PATH shared by every build and test script.
$ErrorActionPreference = 'Stop'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (!$vs) { throw 'Visual Studio C++ Build Tools not found' }
$env:PATH = "$(Split-Path $vswhere);$env:PATH"
Import-Module (Join-Path $vs 'Common7\Tools\Microsoft.VisualStudio.DevShell.dll')
Enter-VsDevShell -VsInstallPath $vs -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64' | Out-Null
$root = Split-Path $PSScriptRoot
$env:CARGO_TARGET_DIR = "$root\temp\cargo-target"
$env:PATH = "$root\temp\build\native\bin;$env:PATH"

function Mount-TauriGeneratedDirectory([string]$projectRoot) {
    $link = Join-Path $projectRoot 'src-tauri/gen'
    $target = Join-Path $projectRoot 'temp/build/tauri-gen'
    if (Test-Path -LiteralPath $link) {
        $item = Get-Item -LiteralPath $link -Force
        if ($item.LinkType -ne 'Junction') {
            # cargo run outside these scripts regenerates gen/ as a real folder.
            # It only holds generated files, so it is replaced by the junction.
            Remove-Item -LiteralPath $link -Recurse -Force
        }
        else { [System.IO.Directory]::Delete($link) }
    }
    New-Item -ItemType Directory -Force -Path $target | Out-Null
    New-Item -ItemType Junction -Path $link -Target $target | Out-Null
    return $link
}

function Dismount-TauriGeneratedDirectory([string]$link) {
    if (Test-Path -LiteralPath $link) {
        $item = Get-Item -LiteralPath $link -Force
        if ($item.LinkType -ne 'Junction') { throw "Refusing to remove non-junction path: $link" }
        [System.IO.Directory]::Delete($link)
    }
}
