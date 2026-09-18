fn main() {
    #[cfg(feature = "gui")]
    tare_tauri::gui::run();

    #[cfg(not(feature = "gui"))]
    eprintln!(
        "tare-desktop was built without the `gui` feature (headless). \
         Build the desktop shell with: cargo build -p tare-tauri --features gui"
    );
}
