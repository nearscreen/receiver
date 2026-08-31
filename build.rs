//! Gives the Windows executable its icon, so it does not turn up in Explorer
//! and on the taskbar as a nameless default. Everything else about the build
//! is ordinary Cargo.

fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=packaging/windows/nearscreen-receiver.ico");
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("packaging/windows/nearscreen-receiver.ico");
        resource.set("FileDescription", "Nearscreen Receiver");
        resource.set("ProductName", "Nearscreen Receiver");
        resource.set("LegalCopyright", "MIT licensed");
        if let Err(e) = resource.compile() {
            // A missing resource compiler is no reason to fail a build; the
            // program simply looks plainer.
            println!("cargo:warning=no icon in the executable: {e}");
        }
    }
}
