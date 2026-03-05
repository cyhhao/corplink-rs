use bindgen::callbacks::{IntKind, ParseCallbacks};
use std::env;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug)]
struct DefineParser;

impl ParseCallbacks for DefineParser {
    fn int_macro(&self, _name: &str, value: i64) -> Option<IntKind> {
        if value >= i32::MIN as i64 && value <= i32::MAX as i64 {
            Some(IntKind::I32)
        } else {
            None
        }
    }
}

fn main() {
    // -----------------------------------------------------------------------
    // 1. Derive BUILD_VERSION from git tag (falls back to CARGO_PKG_VERSION)
    // -----------------------------------------------------------------------
    emit_build_version();

    // -----------------------------------------------------------------------
    // 2. Build frontend (optional — skip if SKIP_FRONTEND=1 or npm not found)
    // -----------------------------------------------------------------------
    build_frontend();

    // -----------------------------------------------------------------------
    // 2. WireGuard C library bindings (existing logic)
    // -----------------------------------------------------------------------

    // Tell cargo to look for shared libraries in the specified directory
    println!("cargo:rustc-link-search=./libwg");

    // Tell cargo to tell rustc to link the shared library.
    println!("cargo:rustc-link-lib=wg");

    // Tell cargo to invalidate the built crate whenever the wrapper changes
    println!("cargo:rerun-if-changed=./libwg/libwg.h");

    // The bindgen::Builder is the main entry point
    // to bindgen, and lets you build up options for
    // the resulting bindings.
    let bindings = bindgen::Builder::default()
        // The input header we would like to generate
        // bindings for.
        .header("./libwg/libwg.h")
        // Tell cargo to invalidate the built crate whenever any of the
        // included header files changed.
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        // parse number define macro as i32 instead of u32
        .parse_callbacks(Box::new(DefineParser))
        // Finish the builder and generate the bindings.
        .generate()
        // Unwrap the Result and panic on failure.
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

/// Emit `BUILD_VERSION` as a compile-time env var.
///
/// - On a tagged commit (CI release): uses the git tag (e.g. `v5.2.0` → `5.2.0`)
/// - Otherwise (local dev build): falls back to `CARGO_PKG_VERSION`
///
/// This lets the binary report the release version without manually syncing
/// Cargo.toml on every tag push.
fn emit_build_version() {
    // Rerun when HEAD changes (new commit, checkout, tag).
    println!("cargo:rerun-if-changed=.git/HEAD");

    let version = Command::new("git")
        .args(["describe", "--tags", "--exact-match"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let tag = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if tag.is_empty() {
                None
            } else {
                // Strip leading 'v' so "v5.2.0" becomes "5.2.0"
                Some(tag.trim_start_matches('v').to_string())
            }
        })
        .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").unwrap());

    println!("cargo:rustc-env=BUILD_VERSION={}", version);
}

fn build_frontend() {
    // Skip if SKIP_FRONTEND=1
    if env::var("SKIP_FRONTEND").unwrap_or_default() == "1" {
        println!("cargo:warning=Skipping frontend build (SKIP_FRONTEND=1)");
        return;
    }

    let web_dir = PathBuf::from("web");
    if !web_dir.join("package.json").exists() {
        println!("cargo:warning=web/package.json not found, skipping frontend build");
        return;
    }

    // Rerun if frontend source changes
    println!("cargo:rerun-if-changed=web/src/");
    println!("cargo:rerun-if-changed=web/index.html");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/vite.config.ts");

    // Install deps if node_modules missing
    if !web_dir.join("node_modules").exists() {
        let status = Command::new("npm")
            .args(["install"])
            .current_dir(&web_dir)
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                println!("cargo:warning=npm install failed with {}", s);
                return;
            }
            Err(e) => {
                println!(
                    "cargo:warning=npm not found, skipping frontend build: {}",
                    e
                );
                return;
            }
        }
    }

    // Build
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(&web_dir)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => println!("cargo:warning=frontend build failed with {}", s),
        Err(e) => println!("cargo:warning=failed to run npm build: {}", e),
    }
}
