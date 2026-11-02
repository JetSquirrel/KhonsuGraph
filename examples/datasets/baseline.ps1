# Times a khonsu command and samples its peak working set, on Windows.
# The memory is the command's own; a plugin it starts is not counted.
#
#   .\examples\datasets\baseline.ps1 -Exe target\release\khonsugraph.exe -Arguments 'query --db graphs\aws --cypher "MATCH (n) RETURN count(n) AS c"'
param(
    [int]$Runs = 3,
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$Arguments
)
$times = @(); $peaks = @()
for ($i = 0; $i -lt $Runs; $i++) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = (Resolve-Path $Exe).Path
    $info.Arguments = $Arguments
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $clock = [System.Diagnostics.Stopwatch]::StartNew()
    $process = [System.Diagnostics.Process]::Start($info)
    $outputTask = $process.StandardOutput.ReadToEndAsync()
    $errorTask = $process.StandardError.ReadToEndAsync()
    $peak = 0
    while (-not $process.HasExited) {
        try { $process.Refresh(); if ($process.PeakWorkingSet64 -gt $peak) { $peak = $process.PeakWorkingSet64 } } catch {}
        Start-Sleep -Milliseconds 5
    }
    $process.WaitForExit()
    $clock.Stop()
    if ($process.ExitCode -ne 0) { throw "exit $($process.ExitCode): $($outputTask.Result)" }
    $times += $clock.Elapsed.TotalMilliseconds
    $peaks += $peak / 1MB
}
$median = { param($values) ($values | Sort-Object)[[math]::Floor($values.Count / 2)] }
"median {0:N0} ms, peak {1:N0} MB, over {2} runs" -f (& $median $times), (& $median $peaks), $Runs
