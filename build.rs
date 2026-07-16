fn main() {
    slint_build::compile("ui/main.slint").unwrap();
    // Embeds the icon + version info into the exe (Explorer, Task Manager, etc.).
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("ui/icon.ico")
        .compile()
        .unwrap();
}
