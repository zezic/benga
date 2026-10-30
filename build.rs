use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=BENGA_UPDATE_PUBLIC_KEY");
    println!("cargo:rerun-if-changed=assets/Info.plist");
    println!("cargo:rerun-if-changed=assets/icons/benga.ico");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let version_core = version.split(['-', '+']).next().unwrap();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = output_dir.join("Info.plist");
        let template = fs::read_to_string(manifest_dir.join("assets/Info.plist"))
            .expect("read macOS bundle metadata");
        fs::write(&plist, template.replace("@VERSION@", version_core))
            .expect("write versioned macOS metadata");
        println!(
            "cargo:rustc-link-arg-bin=benga=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    } else if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let resource = output_dir.join("benga.rc");
        let compiled = output_dir.join("benga.res");
        let icon = manifest_dir.join("assets/icons/benga.ico");
        let numeric_version = format!("{},0", version_core.replace('.', ","));
        fs::write(
            &resource,
            format!(
                r#"1 ICON "{}"
1 VERSIONINFO
FILEVERSION {numeric_version}
PRODUCTVERSION {numeric_version}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "zezic"
      VALUE "FileDescription", "Benga audio visualizer"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "benga"
      VALUE "OriginalFilename", "benga.exe"
      VALUE "ProductName", "Benga"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
                icon.display().to_string().replace('\\', "\\\\")
            ),
        )
        .expect("write Windows icon and version resource");
        let status = Command::new("rc.exe")
            .arg("/nologo")
            .arg("/fo")
            .arg(&compiled)
            .arg(&resource)
            .status()
            .expect("Windows builds require the Windows SDK resource compiler rc.exe on PATH");
        assert!(status.success(), "Windows resource compilation failed");
        println!("cargo:rustc-link-arg-bin=benga={}", compiled.display());
    }
}
