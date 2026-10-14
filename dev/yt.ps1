# InnerTube probe: yt.ps1 <endpoint> '<json body without context>' <outfile>
param([string]$Endpoint, [string]$Body, [string]$Out)
$cookies = Get-Content "$env:LOCALAPPDATA\ytmcross\cookies.txt" | ForEach-Object { $_ -replace '^#HttpOnly_','' } | Where-Object { $_ -and -not $_.StartsWith('#') } | ForEach-Object { $f = $_ -split "`t"; if ($f.Count -ge 7) { @{ n = $f[5]; v = $f[6] } } }
$header = ($cookies | ForEach-Object { "$($_.n)=$($_.v)" }) -join '; '
$sapisid = ($cookies | Where-Object { $_.n -eq 'SAPISID' } | Select-Object -First 1).v
$ts = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$sha = [Security.Cryptography.SHA1]::Create()
$hash = -join ($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes("$ts $sapisid https://music.youtube.com")) | ForEach-Object { $_.ToString('x2') })
$h = { param($sid) -join ($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes("$ts $sid https://music.youtube.com")) | ForEach-Object { $_.ToString('x2') }) }; $hash1 = & $h (($cookies | Where-Object { $_.n -eq '__Secure-1PAPISID' } | Select-Object -First 1).v); $hash3 = & $h (($cookies | Where-Object { $_.n -eq '__Secure-3PAPISID' } | Select-Object -First 1).v)
$ver = "1.$((Get-Date).ToUniversalTime().ToString('yyyyMMdd')).01.00"
$obj = $Body | ConvertFrom-Json
$obj | Add-Member -NotePropertyName context -NotePropertyValue @{ client = @{ clientName = 'WEB_REMIX'; clientVersion = $ver; hl = 'en' } }
$tmp = [IO.Path]::GetTempFileName()
[IO.File]::WriteAllText($tmp, ($obj | ConvertTo-Json -Depth 20 -Compress))
$hf = [IO.Path]::GetTempFileName()
[IO.File]::WriteAllLines($hf, @("Cookie: $header", "Authorization: SAPISIDHASH ${ts}_$hash SAPISID1PHASH ${ts}_$hash1 SAPISID3PHASH ${ts}_$hash3", "Origin: https://music.youtube.com", "X-Origin: https://music.youtube.com", "X-Goog-AuthUser: 0", "Content-Type: application/json"))
& curl.exe -s -A "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36" -H "@$hf" --data-binary "@$tmp" -o $Out "https://music.youtube.com/youtubei/v1/${Endpoint}?prettyPrint=false"
Remove-Item $tmp, $hf
"{0} bytes -> {1}" -f (Get-Item $Out).Length, $Out
