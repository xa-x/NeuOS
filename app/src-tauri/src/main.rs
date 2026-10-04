// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // WebKitGTK renders transparent windows blank on some NVIDIA/X11 setups;
    // this is Tauri's documented workaround. Must run before any GTK init.
    #[cfg(target_os = "linux")]
    std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");

    neuos_lib::run()
}
