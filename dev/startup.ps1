# Startup: process launch -> first frame (trace), and idle memory. Averages 3 runs.
$exe = "$PSScriptRoot\..\target\release\ytmcross.exe"
$log = "$env:LOCALAPPDATA\ytmcross\trace.log"
$env:YTMCROSS_TRACE = "1"
"exe size: {0:N2} MB" -f ((Get-Item $exe).Length / 1MB)
foreach ($i in 1..3) {
  Get-Process ytmcross -ErrorAction SilentlyContinue | Stop-Process -Force -Confirm:$false
  Remove-Item $log -ErrorAction SilentlyContinue
  $sw = [Diagnostics.Stopwatch]::StartNew()
  $p = Start-Process $exe -PassThru
  while ((Get-Content $log -ErrorAction SilentlyContinue | Select-String 'first frame').Count -eq 0 -and $sw.ElapsedMilliseconds -lt 10000) { Start-Sleep -Milliseconds 5 }
  $wall = $sw.ElapsedMilliseconds
  Start-Sleep 4
  $mem = (Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$($p.Id)").WorkingSetPrivate / 1MB
  $lines = Get-Content $log | Where-Object { $_ -match '\[main\] (main: start|shell:)' }
  "run ${i}: launch->first frame (wall) ${wall} ms | idle {0:N1} MB | trace: {1}" -f $mem, (($lines | ForEach-Object { ($_ -replace '\s+\[main\]\s+', ' ').Trim() }) -join ' ; ')
}
Get-Process ytmcross -ErrorAction SilentlyContinue | Stop-Process -Force -Confirm:$false
