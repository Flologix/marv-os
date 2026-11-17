; Marv OS installer hooks (tauri.windows.conf.json > bundle.windows.nsis.installerHooks).
;
; Before files are copied (an update) or removed (an uninstall), every process still running a program from the
; install folder is stopped: marv.exe (a chat's Marv, a team member, a Claude Code's mesh shim), lazygit.exe, the
; desktop itself. Windows can't replace a program that's running, and the installer only closes marv-desktop.exe:
; a marv.exe left running (six orphaned team members on one PC) made it stop at "Error opening file for writing"
; with Abort / Retry / Ignore, and Ignore left the old marv.exe in place, silently, with the new desktop.
;
; Only programs in the install folder: a Marv the person installed elsewhere (npm, cargo) is never touched.
; `$$` is a literal `$` for PowerShell; nsExec runs it without a window.

!macro MARV_STOP_INSTALLED_PROGRAMS
  nsExec::Exec `powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$dir = '$INSTDIR\'; Get-CimInstance Win32_Process | Where-Object { $$_.ExecutablePath -and $$_.ExecutablePath.StartsWith($$dir, [StringComparison]::OrdinalIgnoreCase) } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force -ErrorAction SilentlyContinue }"`
  Pop $0
  ; Their files' handles close a moment after they end.
  Sleep 500
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro MARV_STOP_INSTALLED_PROGRAMS
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro MARV_STOP_INSTALLED_PROGRAMS
!macroend
