use std::{env, path::Path};

use cbindgen::Config;

fn main() {
    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_path = Path::new(&crate_dir).join("dist/include/v5gdb/v5gdb_impl.h");

    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(
            Config::from_file(Path::new(&crate_dir).join("cbindgen.toml"))
                .expect("cbindgen.toml not found"),
        )
        .generate()
        .expect("unable to generate bindings")
        .write_to_file(out_path);

    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=cbindgen.toml");
}
