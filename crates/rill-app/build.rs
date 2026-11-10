//! Puts Rill's icon, name, publisher and version into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/app/rill.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    // Windows wants four numbers; a pre-release suffix is left out of those.
    let core = version.split(['-', '+']).next().unwrap_or("0.0.0");
    let mut numbers: Vec<u16> = core.split('.').filter_map(|n| n.parse().ok()).collect();
    numbers.resize(4, 0);
    let commas = numbers
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let rc = format!(
        r#"1 ICON "rill.ico"
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Mobasity"
      VALUE "FileDescription", "Rill"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "rill"
      VALUE "LegalCopyright", "Copyright (c) 2026 Mobasity"
      VALUE "OriginalFilename", "rill.exe"
      VALUE "ProductName", "Rill"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let rc_path = out.join("rill.rc");
    std::fs::write(&rc_path, rc).expect("writing the resource file in OUT_DIR");
    embed_resource::compile(
        &rc_path,
        embed_resource::ParamsIncludeDirs([std::path::Path::new("assets/app")]),
    )
    .manifest_optional()
    .unwrap_or_else(|e| {
        panic!("Windows resources: couldn't compile the icon and version details ({e}). Install the Windows SDK's resource compiler (rc.exe).")
    });
}
