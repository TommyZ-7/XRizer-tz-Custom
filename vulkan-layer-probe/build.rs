use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for shader in ["dp4a.comp", "coopmat.comp"] {
        let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("shaders")
            .join(shader);
        let output = out_dir.join(format!("{shader}.spv"));
        println!("cargo::rerun-if-changed={}", input.display());
        let status = Command::new("glslc")
            .arg("-O")
            .arg("--target-env=vulkan1.3")
            .arg(&input)
            .arg("-o")
            .arg(&output)
            .status()
            .expect("failed to launch glslc for the Vulkan probe");
        assert!(status.success(), "failed to compile {}", input.display());
    }
}
