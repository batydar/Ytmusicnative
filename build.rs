fn main() {
    println!("cargo:rerun-if-changed=ui/icon.ico");
    println!("cargo:rerun-if-changed=ui/icon.png");
    println!("cargo:rerun-if-changed=ui/icon.rc");

    #[cfg(windows)]
    {
        if let Ok(out_dir) = std::env::var("OUT_DIR") {
            let obj_path = std::path::Path::new(&out_dir).join("icon.o");
            if let Ok(status) = std::process::Command::new("windres")
                .args(["ui/icon.rc", "-O", "coff", "-o"])
                .arg(&obj_path)
                .status()
            {
                if status.success() {
                    println!("cargo:rustc-link-arg-bins={}", obj_path.display());
                }
            }
        }
    }

    slint_build::compile("ui/app.slint").expect("Failed to compile Slint UI");
}
