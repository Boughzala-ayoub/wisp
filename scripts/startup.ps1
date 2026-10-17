# Launches Wisp several times and reports launch-to-first-frame times, as
# measured by Wisp itself from process creation.
#
#   .\scripts\startup.ps1 -Runs 10
param(
    [string]$Exe = "$PSScriptRoot\..\target\release\wisp.exe",
    [int]$Runs = 10
)
$times = foreach ($i in 1..$Runs) {
    $log = [IO.Path]::GetTempFileName()
    $proc = Start-Process -FilePath $Exe -PassThru -RedirectStandardError $log
    $deadline = (Get-Date).AddSeconds(10)
    while (-not (Select-String -Path $log -Pattern "first frame" -Quiet) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 20
    }
    Stop-Process -Id $proc.Id -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 400
    $line = (Get-Content $log | Select-String "first frame").Line
    Remove-Item $log -ErrorAction SilentlyContinue
    if ($line -match '(\d+) ms') { [int]$Matches[1] }
}
$sorted = $times | Sort-Object
$median = if ($sorted.Count % 2) { $sorted[[int]($sorted.Count / 2)] } else { ($sorted[$sorted.Count / 2 - 1] + $sorted[$sorted.Count / 2]) / 2 }
"runs:   $($times -join ', ') ms"
"median: $median ms   min: $($sorted[0]) ms   max: $($sorted[-1]) ms"
