//! One full ABI source from verified official CLAP headers; no handwritten FFI.

use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = args.next().map(PathBuf::from).ok_or("missing official source root")?;
    let output = args.next().map(PathBuf::from).ok_or("missing generated Rust output")?;
    let target = args.next().ok_or("missing explicit Clang C parser target")?;
    if args.next().is_some() { return Err("unexpected generator argument".into()); }
    let target = target.to_str().ok_or("target is not UTF8")?;
    let include = input.join("include");
    if !include.join("clap/all.h").is_file() { return Err("complete verified official all.h is missing".into()); }
    // Owned translation unit avoids treating a pragma-once SDK header as main.
    // The complete official include tree is unchanged and verified separately.
    let header = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wrapper.h");
    if !header.is_file() { return Err("owned wrapper header is missing".into()); }
    let bindings = bindgen::Builder::default()
        .header(header.to_str().ok_or("header path is not UTF8")?)
        .clang_arg("-x").clang_arg("c").clang_arg("-std=c11")
        .clang_arg(format!("--target={target}"))
        .clang_arg(format!("-I{}", include.display()))
        .allowlist_type("(clap|CLAP)_.*")
        .allowlist_var("(clap|CLAP)_.*")
        .allowlist_function("clap_.*")
        // Header-local version/color objects and the inline compatibility helper
        // have no exported symbol. Their Rust value projections are checked against
        // the unchanged official version.h/color.h by an independent C++ oracle.
        .blocklist_item("^CLAP_VERSION$")
        .blocklist_item("^CLAP_COLOR_TRANSPARENT$")
        .blocklist_function("^clap_version_is_compatible$")
        .generate_inline_functions(false)
        .use_core().ctypes_prefix("core::ffi")
        .generate_cstr(true)
        .prepend_enum_name(false)
        .generate_comments(true)
        .derive_debug(true).derive_copy(true)
        .layout_tests(false)
        .formatter(bindgen::Formatter::None)
        .generate()?;
    bindings.write_to_file(&output)?;
    println!("generated_full_official_clap_abi target={target} output={}", output.display());
    Ok(())
}
