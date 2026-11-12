//! Embeds the application icon and version information into the Windows
//! executable, so Explorer, the taskbar and the installer all show them.

fn main() {
    println!("cargo:rerun-if-changed=assets/plic.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/plic.ico");
        resource.set("ProductName", "plic");
        resource.set("FileDescription", "plic");
        if let Err(error) = resource.compile() {
            // A missing resource compiler should not break the build; the
            // executable just ships without an icon.
            println!("cargo:warning=could not embed the Windows icon: {error}");
        }
    }
}
