fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/control.proto");
    tonic_prost_build::compile_protos("proto/control.proto")?;
    Ok(())
}
