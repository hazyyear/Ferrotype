// Embeds the app icon in the Windows .exe so Explorer and the taskbar show it.
// Elsewhere the window icon is set at runtime (see app::app_icon).
fn main() {
    println!("cargo:rerun-if-changed=assets/brand/ferrotype.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/brand/ferrotype.ico");
        if let Err(e) = res.compile() {
            // A missing resource compiler only costs the .exe icon, so don't fail the build.
            println!("cargo:warning=could not embed the app icon: {e}");
        }
    }
}
