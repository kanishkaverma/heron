//! Load the existing Zeron delegation tools into this Pi process only.
use crate::{HarnessError, process::Command, scratch::ScratchDir};

pub(super) fn configure(
    cmd: &mut Command,
    config: &zeron_proto::McpServer,
    scratch: &ScratchDir,
) -> Result<(), HarnessError> {
    let extension = scratch.path().join("zeron-mcp.mjs");
    std::fs::write(&extension, include_str!("mcp.mjs"))?;
    cmd.arg("--extension").arg(extension).env(
        "ZERON_PI_MCP",
        serde_json::to_string(config).expect("serializable MCP config"),
    );
    Ok(())
}
