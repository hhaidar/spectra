//! Windows: embeds the app icon and version information in the executable.
//! Icon resource 1 is also what GPUI uses for the window and taskbar.

fn main() {
    println!("cargo:rerun-if-changed=assets/spectra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap().replace('\\', "/");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<&str> = version.split(['.', '-']).take(3).collect();
    parts.resize(3, "0");
    let numeric = format!("{},{},{},0", parts[0], parts[1], parts[2]);

    let rc = format!(
        r#"1 ICON "{dir}/assets/spectra.ico"

1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "f+e"
      VALUE "FileDescription", "Spectra"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "spectra"
      VALUE "LegalCopyright", "(c) 2026 f+e"
      VALUE "OriginalFilename", "spectra.exe"
      VALUE "ProductName", "Spectra"
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
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("spectra.rc");
    std::fs::write(&path, rc).unwrap();
    embed_resource::compile(&path, embed_resource::NONE).manifest_optional().unwrap();
}
