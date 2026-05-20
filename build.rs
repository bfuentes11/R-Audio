use std::fs;
use std::path::{Path, PathBuf};

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            let src_path = entry.path();
            let dest_path = dst.as_ref().join(entry.file_name());
            if src_path.extension().map_or(false, |ext| ext == "slint") {
                let content = fs::read_to_string(&src_path)?;
                // Replace deprecated rotation-angle to transform-rotation to silence Slint compiler warnings
                let modified = content.replace("rotation-angle", "transform-rotation");
                fs::write(&dest_path, modified)?;
            } else {
                fs::copy(src_path, dest_path)?;
            }
        }
    }
    Ok(())
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let material_dst = out_dir.join("material-1.0");

    // Rerun build script if original files change
    println!("cargo:rerun-if-changed=material-1.0");
    println!("cargo:rerun-if-changed=ui/main_window.slint");

    // Copy and preprocess material-1.0 to OUT_DIR
    copy_dir_all(manifest_dir.join("material-1.0"), &material_dst).unwrap();

    let library_paths = std::collections::HashMap::from([(
        "material".to_string(),
        material_dst.join("material.slint"),
    )]);
    let config = slint_build::CompilerConfiguration::new().with_library_paths(library_paths);
    slint_build::compile_with_config("ui/main_window.slint", config).unwrap();
}