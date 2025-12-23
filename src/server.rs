use crate::configuration::Configuration;
use rmcp_openapi::Server as McpServer;
use std::io;

pub async fn try_init(configuration: Configuration) -> io::Result<()> {
    tracing::info!(%configuration, "starting catalog MCP server");

    let mut mcp_server: McpServer = configuration
        .try_into_server()
        .await
        .map_err(io::Error::other)?;

    mcp_server.load_openapi_spec().map_err(io::Error::other)?;

    tracing::info!(
        toolCount = mcp_server.tool_count(),
        "Successfully loaded tools from OpenAPI specification"
    );
    tracing::debug!(
        tools = %mcp_server.get_tool_names().join(", "),
        stats = %mcp_server.get_tool_stats(),
        "Available tools"
    );

    mcp_server.validate_registry().map_err(io::Error::other)?;

    Ok(())
}
