//! Builds the Vulkan shaders. `glslc` from the Vulkan SDK is used when it is on the path; the SPIR-V checked in
//! next to the sources is used otherwise, so a plain Rust toolchain can build Snowhush too.

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

const SHADERS: [(&str, &str); 13] = [
    ("sprite", "vert"),
    ("sprite", "frag"),
    ("field", "frag"),
    ("glass", "vert"),
    ("glass", "frag"),
    ("pile", "vert"),
    ("pile", "frag"),
    ("sign", "vert"),
    ("sign", "frag"),
    ("pumpkin", "vert"),
    ("pumpkin", "frag"),
    ("fade", "vert"),
    ("fade", "frag"),
];

fn main() {
    let source = PathBuf::from("src/shaders");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("shaders");
    fs::create_dir_all(&out).expect("create shader output folder");
    println!("cargo:rerun-if-changed=src/shaders");
    println!("cargo:rerun-if-env-changed=GLSLC");
    for (name, stage) in SHADERS {
        let input = source.join(format!("{name}.{stage}"));
        let output = out.join(format!("{name}.{stage}.spv"));
        println!("cargo:rerun-if-changed={}", input.display());
        match compile(&input, &output, stage) {
            Ok(()) => {}
            Err(None) => {
                let checked_in = source.join("spv").join(format!("{name}.{stage}.spv"));
                if !checked_in.is_file() {
                    panic!("no glslc to compile {} and no checked-in SPIR-V at {}", input.display(), checked_in.display());
                }
                fs::copy(&checked_in, &output).unwrap_or_else(|error| panic!("copying {}: {error}", checked_in.display()));
            }
            Err(Some(error)) => panic!("compiling {}: {error}", input.display()),
        }
    }
    println!("cargo:rerun-if-changed=src/shaders/globals.glsl");
    println!("cargo:rerun-if-changed=src/shaders/drop.glsl");
}

/// `Err(None)` means there is no compiler to run; `Err(Some(_))` means it ran and rejected the shader.
fn compile(input: &Path, output: &Path, stage: &str) -> Result<(), Option<String>> {
    let compiler = env::var_os("GLSLC").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("glslc"));
    let result =
        match Command::new(&compiler).arg("-I").arg("src/shaders").arg(format!("-fshader-stage={stage}")).arg(input).arg("-o").arg(output).output() {
            Ok(result) => result,
            Err(error) if error.kind() == ErrorKind::NotFound => return Err(None),
            Err(error) => return Err(Some(error.to_string())),
        };
    if result.status.success() { Ok(()) } else { Err(Some(String::from_utf8_lossy(&result.stderr).trim().to_string())) }
}
