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

.EXAMPLE
# Click a point of the window's content (x,y in its pixels, as the app's
# own --screenshot shows them), then capture the content alone.
powershell -ExecutionPolicy Bypass -File tools/screenshot.ps1 -Out edit.png -Clicks '956,233' -Client
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
    [string]$Keys = '',
    # Left clicks before capturing, in order: 'x,y' in the window's content
    # (its client area), in its pixels.
    [string[]]$Clicks = @(),
    # Capture the content alone (no title bar or borders), as --screenshot does.
    [switch]$Client
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
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
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
    foreach ($c in $Clicks) {
        $xy = $c -split ','
        $pt = New-Object NerkaShotWin32+POINT
        $pt.X = [int]$xy[0]; $pt.Y = [int]$xy[1]
        [NerkaShotWin32]::ClientToScreen($p.MainWindowHandle, [ref]$pt) | Out-Null
        [NerkaShotWin32]::SetCursorPos($pt.X, $pt.Y) | Out-Null
        Start-Sleep -Milliseconds 150
        [NerkaShotWin32]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 60
        [NerkaShotWin32]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 700
    }
    if ($Clicks) {
        # The pointer out of the way: no hover highlight or tooltip in the picture.
        [NerkaShotWin32]::SetCursorPos(0, 0) | Out-Null
        Start-Sleep -Milliseconds 500
    }
    [NerkaShotWin32]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
    if ($Client) {
        $cr = New-Object NerkaShotWin32+RECT
        [NerkaShotWin32]::GetClientRect($p.MainWindowHandle, [ref]$cr) | Out-Null
        $o = New-Object NerkaShotWin32+POINT
        [NerkaShotWin32]::ClientToScreen($p.MainWindowHandle, [ref]$o) | Out-Null
        $r.L = $o.X; $r.T = $o.Y; $r.R = $o.X + $cr.R; $r.B = $o.Y + $cr.B
    }
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
