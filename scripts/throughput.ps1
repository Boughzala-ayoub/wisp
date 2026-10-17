# Times `cat` of a large file in a WSL tab of Wisp: how fast output flows from
# a Linux program through WSL, ConPTY and libghostty-vt to the screen.
# Typing is posted to Wisp's window, so nothing needs focus.
#
#   .\scripts\throughput.ps1 -Distro Ubuntu -Lines 12000000
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\wisp.exe",
    [string]$Distro = "Ubuntu",
    [int]$Lines = 12000000
)
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Thr {
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
}
"@
function Send-Text($hwnd, [string]$text) {
    foreach ($ch in $text.ToCharArray()) { [void][Thr]::PostMessageW($hwnd, 0x102, [IntPtr][int]$ch, [IntPtr]1) }
    $lp = [IntPtr](1 -bor (0x1C -shl 16))
    [void][Thr]::PostMessageW($hwnd, 0x100, [IntPtr]0x0D, $lp)
    [void][Thr]::PostMessageW($hwnd, 0x101, [IntPtr]0x0D, [IntPtr]($lp.ToInt64() -bor 0xC0000000L))
}
function Real-Seconds([string]$timeOutput) {
    if ($timeOutput -match 'real\s+(\d+)m([\d.]+)s') { [int]$Matches[1] * 60 + [double]$Matches[2] }
}

wsl.exe -d $Distro -e sh -c "seq 1 $Lines > /tmp/wisp-big.txt; rm -f /tmp/wisp-time.txt"
$bytes = [long](wsl.exe -d $Distro -e sh -c "wc -c < /tmp/wisp-big.txt")
$mb = $bytes / 1MB
$floor = Real-Seconds ((wsl.exe -d $Distro -e bash -c "time cat /tmp/wisp-big.txt > /dev/null" 2>&1) -join "`n")

# Quoted, or Windows splits the command line at the spaces.
$proc = Start-Process -FilePath $Exe -ArgumentList "`"--command=wsl.exe -d $Distro --cd ~`"" -PassThru
while ($proc.MainWindowHandle -eq 0) { Start-Sleep -Milliseconds 50; $proc.Refresh() }
Start-Sleep -Milliseconds 4000  # WSL boots its shell
Send-Text $proc.MainWindowHandle "{ time cat /tmp/wisp-big.txt; } 2> /tmp/wisp-time.txt"
$deadline = (Get-Date).AddMinutes(5)
do {
    Start-Sleep -Milliseconds 500
    $result = (wsl.exe -d $Distro -e sh -c "cat /tmp/wisp-time.txt 2>/dev/null") -join "`n"
} until ($result -match 'real' -or (Get-Date) -gt $deadline)
Stop-Process -Id $proc.Id
$seconds = Real-Seconds $result
"file:       {0:N1} MB, {1:N0} lines" -f $mb, $Lines
"in Wisp:    {0:N2} s  ({1:N1} MB/s)" -f $seconds, ($mb / $seconds)
"no terminal ({0:N2} s) is the floor: cat into /dev/null" -f $floor
