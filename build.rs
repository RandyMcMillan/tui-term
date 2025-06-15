use std::{fs, path::Path, process::Command};

fn main() {
    // Re-run this build script if the script changes.
    println!("cargo:rerun-if-changed=gnostr-proxy.sh");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest_path = Path::new(&out_dir).join("gnostr-proxy.sh");

    // Copy the script to the OUT_DIR.
    fs::copy("gnostr-proxy.sh", &dest_path).expect("Failed to copy gnostr-proxy.sh");

    // Make the copied script executable.
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        Command::new("chmod")
            .arg("+x")
            .arg(&dest_path)
            .status()
            .expect("Failed to make install_script.sh executable");
    }

    // Tell cargo to include the script in the package.
    println!("cargo:rustc-env=INSTALL_SCRIPT={}", dest_path.display());
}
