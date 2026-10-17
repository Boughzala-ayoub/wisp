# Measures typing latency inside Wisp: for each keypress, how long until the
# shell's echo comes back through ConPTY, and until the frame showing it is
# presented. Keys are posted straight to Wisp's window, so nothing needs focus.
#
#   .\scripts\latency.ps1 -Command cmd.exe -Keys 60
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\wisp.exe",
    [string]$Command = "",
    [int]$Keys = 60,
    [int]$IntervalMs = 120
)
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Lat {
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern uint MapVirtualKeyW(uint code, uint type);
}
"@
$env:WISP_TRACE_LATENCY = "1"
$log = [IO.Path]::GetTempFileName()
$arguments = if ($Command) { @("--command=$Command") } else { @() }
$proc = if ($arguments) {
    Start-Process -FilePath $Exe -ArgumentList $arguments -PassThru -RedirectStandardError $log
} else {
    Start-Process -FilePath $Exe -PassThru -RedirectStandardError $log
}
while ($proc.MainWindowHandle -eq 0) { Start-Sleep -Milliseconds 50; $proc.Refresh() }
$hwnd = $proc.MainWindowHandle
Start-Sleep -Milliseconds 2500  # let the shell finish starting
for ($i = 0; $i -lt $Keys; $i++) {
    $vk = 0x41 + ($i % 26)  # a to z
    $scan = [Lat]::MapVirtualKeyW($vk, 0)
    $down = [IntPtr](1 -bor ($scan -shl 16))
    $up = [IntPtr]($down.ToInt64() -bor 0xC0000000L)
    [void][Lat]::PostMessageW($hwnd, 0x100, [IntPtr]$vk, $down)  # WM_KEYDOWN
    [void][Lat]::PostMessageW($hwnd, 0x101, [IntPtr]$vk, $up)    # WM_KEYUP
    Start-Sleep -Milliseconds $IntervalMs
}
Start-Sleep -Milliseconds 500
Stop-Process -Id $proc.Id
Start-Sleep -Milliseconds 300
$echo = @(); $present = @()
foreach ($line in Get-Content $log) {
    if ($line -match 'key->echo ([\d.]+) ms\s+key->present ([\d.]+) ms') {
        $echo += [double]$Matches[1]; $present += [double]$Matches[2]
    }
}
Remove-Item $log -ErrorAction SilentlyContinue
function Stat($values, $p) { $s = $values | Sort-Object; $s[[Math]::Min($s.Count - 1, [int][Math]::Floor($s.Count * $p))] }
$shell = if ($Command) { $Command } else { "default shell" }
"$shell, $($present.Count) keypresses measured"
"key -> echo     median {0:N1} ms   p95 {1:N1} ms" -f (Stat $echo 0.5), (Stat $echo 0.95)
"key -> present  median {0:N1} ms   p95 {1:N1} ms" -f (Stat $present 0.5), (Stat $present 0.95)
