<#
.SYNOPSIS
Launch nerka-gui, screenshot its window to a PNG, then close it.

.DESCRIPTION
Lets an agent (or a human) check the GUI visually without driving it, e.g.
to confirm a layout or rendering fix. Windows only (user32 + System.Drawing).
Only the window started here is closed; other nerka-gui instances are left
alone. Build first: cargo build -p nerka-gui

.EXAMPLE
powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -Out shot.png -AppArgs big.log,--text,ERROR

.EXAMPLE
# Open the About dialog (F1) before capturing; -Keys uses SendKeys syntax.
powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -Out about.png -Keys '{F1}'
#>
param(
    [Parameter(Mandatory)] [string]$Out,
    [string[]]$AppArgs = @(),
    # Defaults to the debug build next to this script (set below: Windows
    # PowerShell 5 leaves $PSScriptRoot empty in parameter defaults).
    [string]$Exe = '',
    # Seconds to wait after the window appears (indexing, first search).
    [int]$Wait = 3,
    [switch]$KeepOpen,
    # Keystrokes sent to the window before capturing (SendKeys syntax, e.g. '{F1}', '^g').
    [string]$Keys = ''
)
$ErrorActionPreference = 'Stop'
if (-not $Exe) { $Exe = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) '..\target\debug\nerka-gui.exe' }
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class NerkaShotWin32 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
# Physical pixels, so the capture matches the window at any display scaling.
[NerkaShotWin32]::SetProcessDPIAware() | Out-Null

# Start-Process rejects an empty -ArgumentList, so pass it only when set.
# It joins the items with spaces as they are, so an item with a space or a
# quote is quoted here (Windows' rules: a quote is \", backslashes before a
# quote or the closing quote doubled), else `--query 'level >= WARN'`
# arrives as three arguments.
function Quote-Arg([string]$a) {
    if ($a -ne '' -and $a -notmatch '[\s"]') { return $a }
    $q = [regex]::Replace($a, '(\\*)"', { param($m) $m.Groups[1].Value * 2 + '\"' })
    $q = [regex]::Replace($q, '(\\+)$', { param($m) $m.Groups[1].Value * 2 })
    return '"' + $q + '"'
}
$start = @{ FilePath = (Resolve-Path $Exe); PassThru = $true }
if ($AppArgs) { $start.ArgumentList = ($AppArgs | ForEach-Object { Quote-Arg $_ }) -join ' ' }
$p = Start-Process @start
try {
    # winit creates a tiny helper window first; wait for the real one.
    $r = New-Object NerkaShotWin32+RECT
    $deadline = (Get-Date).AddSeconds(20)
    do {
        Start-Sleep -Milliseconds 300
        $p.Refresh()
        [NerkaShotWin32]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
    } while (($r.R - $r.L) -lt 300 -and (Get-Date) -lt $deadline -and -not $p.HasExited)
    if ($p.HasExited) { throw "nerka-gui exited early (code $($p.ExitCode))" }
    if (($r.R - $r.L) -lt 300) { throw 'nerka-gui window did not appear' }

    [NerkaShotWin32]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    Start-Sleep -Seconds $Wait
    if ($Keys) {
        (New-Object -ComObject WScript.Shell).SendKeys($Keys)
        Start-Sleep -Milliseconds 700
    }
    [NerkaShotWin32]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
    $w = $r.R - $r.L; $h = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $h
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
    $g.Dispose()
    $bmp.Save([IO.Path]::GetFullPath($Out), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    "Saved ${w}x${h} to $Out"
} finally {
    if (-not $KeepOpen -and -not $p.HasExited) {
        $p.CloseMainWindow() | Out-Null
        if (-not $p.WaitForExit(2000)) { $p.Kill() }
    }
}
