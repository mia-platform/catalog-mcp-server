# Mia-Platform Catalog MCP Server

The Mia-Platform Catalog MCP Server is a [Model Context Protocol (MCP)](https://modelcontextprotocol.io/docs/getting-started/intro) server that provides seamless integration with [Mia-Platform Catalog](https://docs.mia-platform.eu/docs/products/catalog/overview) APIs, enabling advanced automation and interaction capabilities for developers and tools.

## Setup

Most MCP clients require a configuration file to be created or modified to add the MCP server. The configuration file syntax can be different across clients. Please refer to the following links for the latest expected syntax:

- **Windsurf** (<https://docs.windsurf.com/windsurf/mcp>),
- **VSCode** (<https://code.visualstudio.com/docs/copilot/chat/mcp-servers>),
- **Claude Desktop** (<https://modelcontextprotocol.io/quickstart/user>),
- **Cursor** (<https://docs.cursor.com/context/model-context-protocol>).

For a list of clients that support MCP, see [MCP Clients](https://modelcontextprotocol.io/clients).

For a detailed description on how to connect to the remote Mia-Platform Catalog server, refer to the [official documentation](https://docs.mia-platform.eu/docs/products/catalog/usage/catalog-mcp).

## Configuration

The service is configured by a JSON file at `$CONFIGURATION_FOLDER/config.json`. Its JSON Schema is generated at build time and committed at [`schemas/config.schema.json`](./schemas/config.schema.json), which is the authoritative description of every field and default.

Two fields have no usable default and are worth calling out:

- **`server.allowedHosts`** — the hostnames or `host:port` authorities accepted in the inbound `Host` header. It **must not be empty**: the MCP transport defaults to loopback-only validation, so a remote deployment would answer every request `403 Forbidden: Host header is not allowed`. The service refuses to start on an empty list.
- **`engine.baseUrl`** — the API gateway in front of `catalog-engine`, for example `http://api-gateway:8080`. It must **not** address the `catalog-engine` Service directly: every outbound call has to traverse the gateway so the caller's own authorization is evaluated against it. The service refuses to start on a base URL that names the engine Service.

A minimal configuration:

```json
{
  "server": { "allowedHosts": ["catalog-mcp.example.com"] },
  "engine": { "baseUrl": "http://api-gateway:8080" },
  "auth": { "resource": "https://catalog-mcp.example.com/mcp" }
}
```

Configuration is read and validated before the listener binds: a failure exits non-zero with the field path.

### Environment variables

The service accepts the following environment variables:

| Name                 |                       Type                        | Required |                    Default                    | Description                                    |
| :------------------- | :-----------------------------------------------: | :------: | :-------------------------------------------: | :--------------------------------------------- |
| LOG_LEVEL            | `trace` \| `debug` \| `info` \| `warn` \| `error` |          |                    `info`                     | The log level.                                 |
| CONFIGURATION_FOLDER |                     `path`                        |          | the platform config folder, e.g. `~/.config/catalog-mcp-server` | Folder holding `config.json`. |

### CLI options

| Flag                       | Required |                             Default                              | Description                     |
| :------------------------- | :------: | :--------------------------------------------------------------: | :------------------------------ |
| `--config-folder <FOLDER>` |          | `$CONFIGURATION_FOLDER`, else the platform config folder         | Folder holding `config.json`.   |
| `--version`                |          |                                                                  | Print the version and exit.     |

## Contributing

Contributions are welcome! Please check the [Contributing](./CONTRIBUTING.md) for guidelines on local development, standards, and other useful information.
