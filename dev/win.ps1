# Helpers to drive the ytmcross window for testing: screenshot, click, type.
param([string]$Action = "shot", [double]$X = 0, [double]$Y = 0, [string]$Text = "", [string]$Out = "", [int]$Dwell = 400, [double]$X2 = 0, [double]$Y2 = 0)
Add-Type -AssemblyName System.Drawing, System.Windows.Forms
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint x, uint y, uint d, UIntPtr e);
}
"@
$p = Get-Process ytmcross -ErrorAction Stop | Where-Object { $_.MainWindowTitle -like 'ytmcross*' -and $_.MainWindowTitle -notlike '*sign in*' } | Select-Object -First 1
$h = $p.MainWindowHandle
[W]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 150
$r = New-Object W+RECT; [W]::GetClientRect($h, [ref]$r) | Out-Null
$o = New-Object W+POINT; [W]::ClientToScreen($h, [ref]$o) | Out-Null
switch ($Action) {
  "shot" {
    $w = $r.R - $r.L; $hh = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $hh
    $g = [System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($o.X, $o.Y, 0, 0, $bmp.Size)
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png); "saved ${w}x${hh}"
  }
  "click" {
    # X/Y as fractions (<=1) of the client area, or pixels.
    $px = if ($X -le 1) { [int]($o.X + $X * ($r.R - $r.L)) } else { [int]($o.X + $X) }
    $py = if ($Y -le 1) { [int]($o.Y + $Y * ($r.B - $r.T)) } else { [int]($o.Y + $Y) }
    [W]::SetCursorPos($px, $py) | Out-Null; Start-Sleep -Milliseconds $Dwell
    [W]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 40; [W]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    "clicked $px,$py"
  }
  "rclick" {
    $px = [int]($o.X + $X); $py = [int]($o.Y + $Y)
    [W]::SetCursorPos($px, $py) | Out-Null; Start-Sleep -Milliseconds $Dwell
    [W]::mouse_event(8, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 40; [W]::mouse_event(16, 0, 0, 0, [UIntPtr]::Zero)
    "rclicked $px,$py"
  }
  "drag" {
    $px = [int]($o.X + $X); $py = [int]($o.Y + $Y)
    [W]::SetCursorPos($px, $py) | Out-Null; Start-Sleep -Milliseconds 200
    [W]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60
    for ($i = 1; $i -le 20; $i++) { [W]::SetCursorPos([int]($px + ($X2 - $X) * $i / 20), [int]($py + ($Y2 - $Y) * $i / 20)) | Out-Null; Start-Sleep -Milliseconds 25 }
    Start-Sleep -Milliseconds 200; [W]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    "dragged"
  }
  "move" {
    $px = [int]($o.X + $X); $py = [int]($o.Y + $Y); [W]::SetCursorPos($px, $py) | Out-Null; "moved"
  }
  "type" { [System.Windows.Forms.SendKeys]::SendWait($Text); "typed" }
  "wheel" {
    # Scroll for X seconds over point (Y = client y), alternating direction.
    [W]::SetCursorPos([int]($o.X + 600), [int]($o.Y + $Y)) | Out-Null
    $end = (Get-Date).AddSeconds($X); $i = 0
    while ((Get-Date) -lt $end) { $d = if ((($i / 15) % 2) -lt 1) { [uint32]4294967176 } else { [uint32]120 }; [W]::mouse_event(0x0800, 0, 0, $d, [UIntPtr]::Zero); Start-Sleep -Milliseconds 16; $i++ }
    $p.Refresh(); "title: $($p.MainWindowTitle)"
  }
}
