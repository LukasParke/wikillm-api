//! MCP stdio server binary (`wikillm-mcp`).

fn main() -> wikillm_api::error::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(wikillm_api::mcp::run())
}
