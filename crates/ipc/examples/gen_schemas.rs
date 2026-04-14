//! Regenerate the JSON Schema files in `schemas/`.
//!
//! Run via `just schemas` (which passes the `json-schema` feature).

use std::path::PathBuf;

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schemas")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = schemas_dir();
    std::fs::create_dir_all(&dir)?;

    let c2h = schemars::schema_for!(forgeclaw_ipc::ContainerToHost);
    let json = serde_json::to_string_pretty(&c2h)?;
    std::fs::write(
        dir.join("container_to_host.schema.json"),
        format!("{json}\n"),
    )?;

    let h2c = schemars::schema_for!(forgeclaw_ipc::HostToContainer);
    let json = serde_json::to_string_pretty(&h2c)?;
    std::fs::write(
        dir.join("host_to_container.schema.json"),
        format!("{json}\n"),
    )?;

    Ok(())
}
