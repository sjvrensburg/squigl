fn main() {
    // ONNX Runtime's WebGPU provider is a separate library `ort` copies next to the
    // binary (`libwebgpu_dawn.so`, `.dylib`); look there first, as the egui window
    // does. Windows finds a DLL next to the exe anyway.
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux") => println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN"),
        Ok("macos") => {
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path");
            println!("cargo:rustc-link-arg-bins=-Wl,-rpath,@executable_path/../Frameworks");
        }
        _ => {}
    }
    tauri_build::build()
}
