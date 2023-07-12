use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

// Inspiration from https://github.com/dtolnay/cxx/blob/b3fcc11c5ec218f7dbcd3ac6b961953c69efa2b6/gen/build/src/target.rs#L10
pub(crate) fn find_target_dir(out_dir: impl AsRef<Path>) -> Option<PathBuf> {
    let mut dir = out_dir.as_ref().to_path_buf();

    loop {
        if dir.join(".rustc_info.json").exists()
            || dir.join("CACHEDIR.TAG").exists()
            || dir.file_name() == Some(OsStr::new("target"))
                && dir
                    .parent()
                    .map_or(false, |parent| parent.join("Cargo.toml").exists())
        {
            return Some(dir);
        }

        if dir.pop() {
            continue;
        }

        return None;
    }
}

fn main() {
    // Unfortunately, cargo does not supply target directory information for
    // procedural macros. This attempts to process the `OUT_DIR` environment
    // variable to find the root target directory.
    println!(
        "cargo:rustc-env=CARGO_TARGET_DIR={dir}",
        dir = find_target_dir(
            std::env::var("OUT_DIR").expect("failed to get `OUT_DIR` environment variable")
        )
        .expect("failed to find target directory")
        .display()
    )
}
