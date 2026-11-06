//! Puts Rill's icon into the Windows executable, for Explorer, the taskbar and the window.

fn main() {
    let dir = std::path::Path::new("assets/app");
    println!("cargo:rerun-if-changed=assets/app/rill.rc");
    println!("cargo:rerun-if-changed=assets/app/rill.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile(dir.join("rill.rc"), embed_resource::ParamsIncludeDirs([dir]))
            .manifest_optional()
            .unwrap_or_else(|e| {
                panic!("Windows icon: couldn't compile assets/app/rill.rc ({e}). Install the Windows SDK's resource compiler (rc.exe).")
            });
    }
}
