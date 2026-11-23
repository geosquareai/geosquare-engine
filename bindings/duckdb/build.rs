//! Windows GNU (MinGW) builds: aws-lc marks some of its symbols dllexport, after which the
//! linker exports only dllexport symbols and drops the extension entry point. Export it
//! explicitly with a .def file so DuckDB finds `geosquare_v2_init_c_api`.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "gnu" {
        let out =
            std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("geosquare_v2.def");
        std::fs::write(&out, "EXPORTS\n    geosquare_v2_init_c_api\n").unwrap();
        println!("cargo:rustc-cdylib-link-arg={}", out.display());
    }
}
