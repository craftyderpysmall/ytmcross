# Does memory plateau? Several searches + plays, then idle. Task Manager's metric (private working set).
$s = $PSScriptRoot
$env:Path = "$env:USERPROFILE\scoop\shims;$env:Path"
Get-Process ytmcross -ErrorAction SilentlyContinue | Stop-Process -Force -Confirm:$false
Remove-Item Env:YTMCROSS_TRACE -ErrorAction SilentlyContinue
$p = Start-Process "$PSScriptRoot\..\target\release\ytmcross.exe" -PassThru
function mem($label) { "{0,-28} {1,5:N1} MB" -f $label, ((Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$($p.Id)").WorkingSetPrivate / 1MB) }
Start-Sleep 5; mem "startup"
foreach ($q in 'daft punk', 'radiohead', 'kendrick lamar', 'taylor swift') {
  & "$s\win.ps1" -Action click -X 600 -Y 82 | Out-Null
  [System.Windows.Forms.SendKeys]::SendWait("^a") 2>$null
  & "$s\win.ps1" -Action type -Text "^a$q{ENTER}" | Out-Null
  Start-Sleep 5; mem "after search '$q'"
}
& "$s\win.ps1" -Action click -X 600 -Y 150 -Dwell 200 | Out-Null; Start-Sleep 6; mem "playing track 1"
& "$s\win.ps1" -Action click -X 600 -Y 278 -Dwell 200 | Out-Null; Start-Sleep 6; mem "playing track 2"
foreach ($t in 1..4) { Start-Sleep 15; mem "idle +$($t * 15)s (playing)" }
& "$s\win.ps1" -Action click -X 625 -Y 668 | Out-Null; Start-Sleep 3; mem "paused"
