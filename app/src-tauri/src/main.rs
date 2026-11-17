// No console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // Marv runs itself for its hidden helpers (`__relay`, `__run-checks`, `__html-convert`, `__guard`), often inside a
    // sandbox with no display: answer them first, before the re-exec below and before Tauri (mk-60m.10: `process
    // start` used to start the app in the sandbox, which died with "Failed to initialize gtk backend").
    if let Some(code) = marv_session::helper_modes::run_from_env() {
        return code;
    }
    // macOS: opened from Finder or the Dock the app has launchd's bare PATH and no LANG, which every agent terminal
    // would inherit; run again with the login shell's (`marv_desktop::login_env`). Nothing elsewhere.
    marv_desktop::login_env::reexec_with_login_env();
    // WebKitGTK's DMA-BUF renderer kills the window at start on some Wayland + NVIDIA setups ("Error 71 (Protocol
    // error) dispatching to Wayland display"), but without it every frame is copied through the CPU: terminals
    // redrawing as you select text lagged. So Marv runs itself again with the renderer first, and only if the window
    // dies at start that way, again without it, remembered for the next launches (`marv_desktop::dmabuf`). Setting
    // the variable needs `unsafe` in Rust 2024, hence the re-run; set it yourself to choose.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
        && let Ok(exe) = std::env::current_exe()
    {
        std::process::exit(marv_desktop::dmabuf::run_with_fallback(&exe));
    }
    // mk-ssf: the helpers above run inside the sandbox, where this binary's bundled WebKitGTK/GTK libraries (the
    // AppImage's `usr/lib`) aren't visible: the bundles ship the plain `marv` CLI for them (`usr/libexec/marv-os/marv`,
    // no GTK), so sessions mount and run that one. A build without it (cargo, `tauri dev`) answers them itself.
    if let Some(helper) = std::env::current_exe().ok().and_then(|exe| marv_session::helper_modes::bundled_helper(&exe)) {
        marv_session::helper_modes::set_helper_exe(helper);
    }
    marv_desktop::run();
    std::process::ExitCode::SUCCESS
}
