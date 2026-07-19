fn main() -> Result<(), Box<dyn std::error::Error>> {
    // protox compiles the .proto in pure Rust (no system protoc), producing a
    // FileDescriptorSet that prost-build turns into Rust types in OUT_DIR.
    let file_descriptors = protox::compile(["proto/ue_caps.proto"], ["proto"])?;
    prost_build::Config::new().compile_fds(file_descriptors)?;
    println!("cargo:rerun-if-changed=proto/ue_caps.proto");
    Ok(())
}
