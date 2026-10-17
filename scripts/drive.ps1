# Launches Wisp, types into it, and screenshots its window.
# Used to check each milestone by eye and to capture DEVLOG screenshots.
#
#   .\scripts\drive.ps1 -Out shot.png -Text "dir`n" -Wait 1500 -Size 900x600
#
# Input is posted straight to Wisp's window (WM_CHAR, and WM_KEYDOWN for
# Enter), so it never needs focus and can't leak into another app. The capture
# uses PrintWindow, which works even when other windows cover Wisp.
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\wisp.exe",
    [Parameter(Mandatory)][string]$Out,
    [string]$Text = "",
    [int]$Wait = 1500,
    [string]$Size = "",
    [switch]$Focus,
    [switch]$Keep
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class Win {
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@
[void][Win]::SetProcessDpiAwarenessContext([IntPtr]-4)  # physical pixels

$log = [IO.Path]::GetTempFileName()
$proc = Start-Process -FilePath $Exe -PassThru -RedirectStandardError $log
try {
    $deadline = (Get-Date).AddSeconds(10)
    while ($proc.MainWindowHandle -eq 0) {
        if ((Get-Date) -gt $deadline) { throw "no window appeared" }
        Start-Sleep -Milliseconds 50
        $proc.Refresh()
    }
    $hwnd = $proc.MainWindowHandle
    if ($Size) {
        $w, $h = $Size -split "x"
        [void][Win]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, [int]$w, [int]$h, 0x16)  # NOMOVE|NOZORDER|NOACTIVATE
    }
    # Draw the focused look (solid cursor) without stealing real focus.
    if ($Focus) { [void][Win]::PostMessageW($hwnd, 0x0007, [IntPtr]::Zero, [IntPtr]::Zero) }  # WM_SETFOCUS
    Start-Sleep -Milliseconds 800  # let the shell print its prompt
    foreach ($ch in $Text.ToCharArray()) {
        if ($ch -eq "`n") {
            $lp = [IntPtr](1 -bor (0x1C -shl 16))  # repeat 1, scan code of Enter
            [void][Win]::PostMessageW($hwnd, 0x0100, [IntPtr]0x0D, $lp)  # WM_KEYDOWN VK_RETURN
            [void][Win]::PostMessageW($hwnd, 0x0101, [IntPtr]0x0D, [IntPtr]($lp.ToInt64() -bor 0xC0000000L))
        } else {
            [void][Win]::PostMessageW($hwnd, 0x0102, [IntPtr][int]$ch, [IntPtr]1)  # WM_CHAR
        }
    }
    Start-Sleep -Milliseconds $Wait
    $proc.Refresh()
    if ($proc.HasExited) { Write-Output "wisp exited (code $($proc.ExitCode)); no screenshot"; return }

    $r = New-Object Win+RECT
    [void][Win]::GetWindowRect($hwnd, [ref]$r)
    $bmp = New-Object System.Drawing.Bitmap ($r.Right - $r.Left), ($r.Bottom - $r.Top)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $hdc = $g.GetHdc()
    [void][Win]::PrintWindow($hwnd, $hdc, 2)  # PW_RENDERFULLCONTENT: includes DirectX content
    $g.ReleaseHdc($hdc)
    $bmp.Save((Join-Path (Get-Location) $Out), [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Output "saved $Out ($($bmp.Width)x$($bmp.Height))"
    $proc.Refresh()
    Write-Output "wisp still running: $(-not $proc.HasExited)"
} finally {
    if (-not $Keep -and -not $proc.HasExited) { Stop-Process -Id $proc.Id }
    Start-Sleep -Milliseconds 200
    Get-Content $log
    Remove-Item $log -ErrorAction SilentlyContinue
}
