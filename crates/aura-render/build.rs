use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    for shader in [
        "background.vert",
        "background.frag",
        "text.vert",
        "text.frag",
    ] {
        let source = PathBuf::from("../../shaders").join(shader);
        let target = out.join(format!("{shader}.spv"));
        let status = Command::new("glslc")
            .arg("--target-env=vulkan1.3")
            .arg(&source)
            .arg("-o")
            .arg(&target)
            .status()
            .unwrap_or_else(|error| panic!("cannot run glslc for {}: {error}", source.display()));
        assert!(
            status.success(),
            "glslc failed to compile {}",
            source.display()
        );
        println!("cargo:rerun-if-changed={}", source.display());
    }
}
