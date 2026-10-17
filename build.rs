//! Embeds the app icon and version info into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/chitra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/chitra.ico")
            .set("FileDescription", "Chitra photo viewer and converter")
            .set("ProductName", "Chitra");
        if let Err(e) = res.compile() {
            // A missing resource compiler only costs the Explorer icon.
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
