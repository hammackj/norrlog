// Included by the build scripts of norrlog and norrlogd (`include!`), so the
// two Windows programs carry the same version resource: product "Norrlog" and
// the workspace version, which code signing checks against (docs/code-signing.md).
//
// The resource is written into OUT_DIR rather than kept as a .rc file because
// the version comes from Cargo, and rc has no way to ask Cargo for it.

/// Embed a VERSIONINFO, and the icon when `icon` is given, into the program
/// being built. Does nothing unless the target is Windows.
fn embed_version_info(description: &str, original_name: &str, icon: Option<&str>) {
    println!("cargo:rerun-if-changed=../../packaging/windows");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let mut parts = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../packaging/windows");
    let icon_line = match icon {
        Some(name) => {
            let path = dir.join(name).canonicalize().expect("the Windows icon");
            // rc reads backslashes as escapes inside a string.
            format!(
                "1 ICON \"{}\"\n",
                path.display().to_string().replace('\\', "/")
            )
        }
        None => String::new(),
    };
    let rc = format!(
        r#"{icon_line}
// 1 is VS_VERSION_INFO; the constants below are spelled as numbers so no
// SDK header is needed.
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "hammackj"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{original_name}"
      VALUE "LegalCopyright", "Copyright (c) hammackj. MIT licence."
      VALUE "OriginalFilename", "{original_name}"
      VALUE "ProductName", "Norrlog"
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
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("version.rc");
    std::fs::write(&out, rc).expect("writing version.rc");
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_optional()
        .expect("embedding the Windows version resource");
}
