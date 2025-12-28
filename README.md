# Mia-Platform Catalog MCP Server

The Mia-Platform Catalog MCP Server is a [Model Context Protocol (MCP)](https://modelcontextprotocol.io/docs/getting-started/intro) server that provides seamless integration with Mia-Platform Catalog APIs, enabling advanced automation and interaction capabilities for developers and tools.

## How to run

The service acts as a proxy to the Mia-Platform Catalog, automatically generating the available tools from the Catalog OpenAPI specification.

Therefore, the service needs to known the URL on which the Catalog can be reached (`--base-url` [CLI flag](#cli-options)), and the OpenAPI specification to use (`--spec` [CLI flag](#cli-options)). By default, the spec is fetched from the Catalog itself. Otherwise, you can instruct the service to source it from a path on the filesystem or from a remote URL.

### Environment variables

The service accepts the following environment variables:

| Name      |                       Type                        | Required | Default | Description    |
| :-------- | :-----------------------------------------------: | :------: | :-----: | :------------- |
| LOG_LEVEL | `trace` \| `debug` \| `info` \| `warn` \| `error` |          | `info`  | The log level. |

### CLI options

The service can be configured with the following set of CLI arguments:

| Flag                      | Required |          Default          | Description                                                                                    |
| :------------------------ | :------: | :-----------------------: | :--------------------------------------------------------------------------------------------- |
| `-s`, `--spec <LOCATION>` |          | `<base-url>/openapi/json` | Path or URL to the OpenAPI specification file from which the MCP server should be built.       |
| `-b`, `--base-url <URL>`  |    ✓     |                           | Mia-Platform Catalog base URL.                                                                 |
| `--stdio`                 |          |         `false`           | Use stdio transport instead of HTTP streaming. When enabled, the server runs in stdio mode.    |
| `--api-prefix <PREFIX>`   |          |            `/`            | Prefix for the MCP server REST API (only applicable in HTTP mode).                            |
| `-p`, `--port <PORT>`     |          |          `8000`           | Port to bind the MCP server to (only applicable in HTTP mode).                                |
| `--ip <IP>`               |          |         `0.0.0.0`         | IP address to bind the MCP server to (only applicable in HTTP mode).                          |

### Run with Docker

### Run from source

```json
{
  "servers": {
    "catalog": {
      "type": "stdio",
      "command": "cargo",
      "args": [
        "run",
        "--",
        "--stdio",
        "--base-url=http://localhost:3000",
        "--spec=http://localhost:3000/api/openapi/json"
      ]
    }
  },
  "inputs": []
}
```

```json
{
  "servers": {
    "catalog": {
      "type": "stdio",
      "command": "./target/release/catalog-mcp-server",
      "args": [
        "--stdio",
        "--base-url=http://localhost:3000",
        "--spec=http://localhost:3000/api/openapi/json"
      ]
    }
  },
  "inputs": []
}
```

```json
{
  "servers": {
    "catalog": {
      "type": "stdio",
      "command": "docker",
      "args": [
        "run",
        "-i",
        "--rm",
        "--name", "catalog-mcp-server",
        "ghcr.io/mia-platform/catalog-mcp-server:latest",
        "--stdio",
        "--base-url=http://host.docker.internal:3000",
        "--spec=http://host.docker.internal:3000/api/openapi/json"
      ]
    }
  },
  "inputs": []
}
```
