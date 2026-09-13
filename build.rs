use std::io::Result;

fn main() -> Result<()> {
    tonic_build::configure().compile_protos(&["proto/aegis/v1/aegis.proto"], &["proto/"])?;
    Ok(())
}
