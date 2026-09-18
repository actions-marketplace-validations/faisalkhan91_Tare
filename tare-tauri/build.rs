fn main() {
    // Only invoke the Tauri build step when the GUI feature is enabled. The default
    // headless build (and scripts/ci.sh) skips it entirely.
    #[cfg(feature = "gui")]
    tauri_build::build();
}
