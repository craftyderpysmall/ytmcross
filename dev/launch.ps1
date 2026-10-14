# Restart ytmcross and bring it to the front.
Get-Process ytmcross -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300
Start-Process "$PSScriptRoot\..\target\release\ytmcross.exe"
Start-Sleep 2
Add-Type -Name U -Namespace N -MemberDefinition '[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h,int c); [DllImport("user32.dll")] public static extern void keybd_event(byte k,byte s,uint f,UIntPtr e);' -ErrorAction SilentlyContinue
$p = Get-Process ytmcross | Where-Object MainWindowTitle | Select-Object -First 1
[N.U]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [N.U]::ShowWindow($p.MainWindowHandle, 9) | Out-Null; [N.U]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
"launched $($p.Id)"
