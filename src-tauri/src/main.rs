// Tauri v2 builds `litemusicdl_lib` as a library plus this thin `main` binary.
// On Windows a Rust binary defaults to the `console` subsystem, which opens a
// black terminal window next to the GUI. `windows_subsystem = "windows"` (set
// only for release builds) makes the OS launch the app without a console.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    litemusicdl_lib::run();
}
