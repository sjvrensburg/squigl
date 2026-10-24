fn main() {
    // ONNX Runtime's WebGPU provider lives in a separate library (`libwebgpu_dawn.so`
    // on Linux) that `ort` copies next to the binary; look there first so a release
    // tarball (binary + models/) runs from any directory without LD_LIBRARY_PATH.
    // Windows needs nothing: a DLL next to the exe is found anyway.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN"),
        // Next to the binary, and in an app bundle's Frameworks.
        Ok("macos") => {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path");
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
        }
        _ => {}
    }
}
