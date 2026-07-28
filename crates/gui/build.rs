use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=../ui/ui/assets/icon.png");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let png = fs::read("../ui/ui/assets/icon.png").expect("read launcher icon");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert!(width <= 256 && height <= 256, "launcher icon is too large");

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut ico = Vec::with_capacity(22 + png.len());
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    ico.extend_from_slice(&[(width % 256) as u8, (height % 256) as u8, 0, 0, 1, 0, 32, 0]);
    ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22_u32.to_le_bytes());
    ico.extend_from_slice(&png);
    fs::write(out.join("icon.ico"), ico).expect("write launcher ico");
    fs::write(out.join("icon.rc"), "1 ICON \"icon.ico\"\n").expect("write icon resource");

    let object = out.join("icon.o");
    let status = Command::new("windres")
        .current_dir(&out)
        .args(["--input", "icon.rc", "--output-format", "coff", "--output"])
        .arg(&object)
        .status()
        .expect("run windres");
    assert!(status.success(), "windres failed");
    println!("cargo:rustc-link-arg-bin=hmcl-gui={}", object.display());
}
