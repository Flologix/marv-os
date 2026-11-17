# Launches a built marv-desktop.exe and waits for it to say it's up: the window opened, the bundled page ran and
# asked the backend for the launch folder (MARV_DESKTOP_SMOKE_TEST=1, see take_launch in app/src-tauri/src/lib.rs).
# The Windows twin of smoke-macos.sh. Fails if it doesn't say so within the time limit, or exits without it.
#
#   pwsh packaging/desktop/smoke-windows.ps1 path\to\marv-desktop.exe
param([Parameter(Mandatory = $true)][string]$Exe)
$ErrorActionPreference = "Stop"
$limit = if ($env:MARV_DESKTOP_SMOKE_SECONDS) { [int]$env:MARV_DESKTOP_SMOKE_SECONDS } else { 120 }

$work = Join-Path ([IO.Path]::GetTempPath()) ("marv-desktop-smoke-" + [guid]::NewGuid())
$folder = Join-Path $work "folder"
$home_ = Join-Path $work "home"
# A profile laid out as a real one: Windows resolves the app's folders (RoamingAppData, LocalAppData) through the
# registry's %USERPROFILE%\AppData\..., and a missing one is "unknown" (the first run failed its setup that way).
$roaming = Join-Path $home_ "AppData\Roaming"
$local = Join-Path $home_ "AppData\Local"
New-Item -ItemType Directory -Force -Path $roaming, $local, $folder | Out-Null
$out = Join-Path $work "out.log"
$err = Join-Path $work "err.log"

# A profile of its own, so the app's settings and chats land in the temp folder (a GUI-subsystem exe still writes to
# handles its parent redirected, so the ready line reaches the log).
$env:USERPROFILE = $home_
$env:HOME = $home_
$env:APPDATA = $roaming
$env:LOCALAPPDATA = $local
$env:MARV_DESKTOP_SMOKE_TEST = "1"
$p = Start-Process -FilePath $Exe -ArgumentList "`"$folder`"" -PassThru -RedirectStandardOutput $out -RedirectStandardError $err

$ready = $false
$waited = 0
while (-not $p.HasExited -and $waited -lt $limit) {
  if ((Test-Path $out) -and (Select-String -Path $out -Pattern '^marv-desktop: ready' -Quiet)) { $ready = $true; break }
  Start-Sleep -Seconds 1
  $waited++
}
if (-not $ready -and (Test-Path $out)) { $ready = Select-String -Path $out -Pattern '^marv-desktop: ready' -Quiet }
if (-not $p.HasExited) {
  # Asked to quit after printing: give it a moment, then stop it and what it started.
  Start-Sleep -Seconds 5
  if (-not $p.HasExited) { & taskkill /PID $p.Id /T /F | Out-Null }
}

Write-Host "== $Exe output"
if (Test-Path $out) { Get-Content $out }
if (Test-Path $err) { Get-Content $err }
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
if (-not $ready) { Write-Error "Marv desktop didn't reach ready within ${limit}s"; exit 1 }
Write-Host "Marv desktop reached ready in ${waited}s"
