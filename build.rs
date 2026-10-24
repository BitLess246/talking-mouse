fn main() {
    // Give the Windows executables the app icon and version info.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icon/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/icon.ico")
            .set("ProductName", "Talking Mouse")
            .set("FileDescription", "Talking Mouse");
        if let Err(e) = res.compile() {
            println!("cargo:warning=could not embed the Windows icon: {e}");
        }
    }
}
