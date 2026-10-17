//! Embeds the app icon and version info into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/chtra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/chtra.ico")
            .set("FileDescription", "Chtra photo viewer and converter")
            .set("ProductName", "Chtra");
        if let Err(e) = res.compile() {
            // A missing resource compiler only costs the Explorer icon.
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
