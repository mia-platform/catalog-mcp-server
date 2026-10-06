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

> [!NOTE]
> This version replaces the server generated from the engine's OpenAPI document. The `--spec` and `--base-url` flags are gone: the server is configured by a `config.json` (see [Configuration](#configuration)). A deployment that still passes those flags — `catalog-helm-chart` up to `v0.3.57` does — must be updated before it runs this image.

To try the server on your machine against a local Catalog, see [CONTRIBUTING.md](./CONTRIBUTING.md#2-setup-the-development-environment): `cargo make dev_up`, then `cargo make dev`.

## Tools

| Tool | What it does |
| :--- | :--- |
| `list_catalog_types` | Lists every item type, with the `kind`, `group`, `family` and `version` needed to address its items, and its short `description` when it has one. A type with an agent briefing (`llmDescription`) is flagged `hasLlmDescription` — the briefing itself is read with `get_item_schema` and `fields: []`, so a full listing stays small however long briefings get. `search` narrows by name or purpose, briefings included; a row that matched only in its briefing carries `matchedOn: "llmDescription"`. An empty `search` is refused. |
| `search_catalog` | Searches items: free text (`query`), one type (`kind`, plus `group` when several types share the kind), exact `labels` and `fields` filters. `fields` takes `metadata.owner` (an e-mail or principal id), and `null` as a value matches an unset field: `{"metadata.owner": null}` finds items without an owner, and a `spec.` field can be `null` when `kind` is given. Rows carry the record's `owner` and `recordUpdatedAt` when set. Paginated with an opaque `cursor`. With `kind: "Relationship"`, each row also names the relationship's `type`, `source` and `target` (each as `name`, `kind`, `group`), read in the same call; filter them by an end with `fields: {"spec.targetRef": <urn>}`, the `urn` `describe_item` returns. A kind-less search on a `spec.` field warns that types not declaring it filterable are left out. |
| `delete_item_type` | Deletes one type definition **and every item of it**, with their history and every relationship touching them. Guarded by the item count: while the type has items, the first call is refused (as an answer, not an error) with the count, and only a call passing that exact number as `expected_items` deletes. The count is re-taken on every call, and a count that cannot be made stops the delete. |
| `describe_item` | One item by name, with its relationships in the same answer. `kind`/`group` only when the name is ambiguous; a shared `kind` without its `group`, or a `group` alone, is completed by the name when only one item matches, and the answer says how. Relationships can be restricted, grouped and paged. The answer also carries the item's `urn` and the catalog record's `owner` and `recordCreatedAt`/`recordUpdatedAt`, and each relationship names the related item's `group` and `title`. |
| `get_item_schema` | One type's whole definition, including the schema its items follow. With `fields` (e.g. `["spec.lifecycle"]`) it returns only those fields' schema, together with the type's `llmDescription` and short `description`; with `fields: []` it returns only the type and those two descriptions, no schema. |
| `apply_item` | Creates or updates one item. Send only what changes: the server reads the item, merge-patches it (RFC 7396: `null` removes a field, lists are replaced whole) and writes it back, so nothing else is lost. Answers with `created`, the `changed` field paths and whether a conflict was `retried`. Custom fields are not written here. |
| `apply_item_type` | Creates or updates one type definition, merge-patched like `apply_item` (`spec.versions` is replaced whole, so start from `get_item_schema`'s `spec`). Fields fixed at creation — `kind`, `plural`, `group`, history and audit — are listed in `ignored` rather than silently dropped; the answer also says whether the items' schema changed (with the count of those the caller can read), which served versions were removed, and which background jobs a creation started. A new type needs `spec.group`, `spec.names.plural` and `spec.versions`. |
| `delete_item` | Deletes one item permanently, guarded by the `resourceVersion` it reads first: a concurrent change is a `conflict`, never an overwrite. Its relationships in both directions and its revision history go with it; the answer counts the relationships `describe_item` lists for the caller (`"200+"` beyond one page) — behind the API gateway the engine hides some from the listing, but the delete removes them all — and relays the engine's warning when the cleanup failed. A name shared by several types deletes nothing until `kind` says which; a shared `kind` without its `group`, or a `group` alone, deletes only when one item of that name matches, and says how it was found. |
| `list_tenants` | The tenants the caller can access, and the one it is working in. |

Every tool reads and writes with the **caller's own identity**. The server forwards `x-mia-acl-context` and `x-mia-principal-id` to the engine; behind the Mia-Platform API gateway they are set for you, while a client talking to the server directly (a local setup, for instance) must send `x-mia-acl-context` itself.

## Configuration

The service is configured by a JSON file at `$CONFIGURATION_FOLDER/config.json`. Its JSON Schema is generated at build time and committed at [`schemas/config.schema.json`](./schemas/config.schema.json), which is the authoritative description of every field and default.

Three fields have no usable default:

- **`server.allowedHosts`** — the hostnames or `host:port` authorities accepted in the inbound `Host` header. It **must not be empty**: the MCP transport defaults to loopback-only validation, so a remote deployment would answer every request `403 Forbidden: Host header is not allowed`.
- **`engine.baseUrl`** — the API gateway in front of `catalog-engine`, for example `http://api-gateway:8080`. It must **not** address the `catalog-engine` Service directly: every outbound call has to traverse the gateway so the caller's own authorization is evaluated against it. A base URL whose host is `catalog-engine` is refused.
- **`auth.resource`** — this server's public MCP URL, exactly as the gateway publishes it in its protected-resource metadata. It must be a canonical URI: a scheme, no fragment, no trailing slash.

A minimal configuration:

```json
{
  "server": { "allowedHosts": ["catalog-mcp.example.com"] },
  "engine": { "baseUrl": "http://api-gateway:8080" },
  "auth": { "resource": "https://catalog-mcp.example.com/mcp" }
}
```

Configuration is read and validated before the listener binds: a failure exits non-zero with the field path. Besides the three fields above, the service refuses to start on `auth.mode: "resource-server"` (not implemented yet; `gateway` is the only mode) and on a `tools.callDeadlineSeconds` shorter than `engine.timeoutMs + engine.connectTimeoutMs`, which would make every call time out at the wrong layer.

Some capabilities ship **disabled** and are switched on in configuration: the per-tenant rate limiter (`tools.rateLimit.enabled`) and structured tool output (`response.structuredContent`).

### Endpoints

| Path | Description |
| :--- | :--- |
| `server.mcpPath` (default `/mcp`) | The MCP endpoint (Streamable HTTP). |
| `/-/healthz` | Liveness. |
| `/-/ready` | Readiness; also probes the engine when `health.readinessChecksEngine` is `true` (the default). |
| `/-/metrics` | Prometheus metrics, when `observability.metricsEnabled` is `true` (the default). A metric family appears once it has its first sample. |

### Environment variables

The service accepts the following environment variables:

| Name                 |                       Type                        | Required |                    Default                    | Description                                    |
| :------------------- | :-----------------------------------------------: | :------: | :-------------------------------------------: | :--------------------------------------------- |
| LOG_LEVEL            | `trace` \| `debug` \| `info` \| `warn` \| `error` |          |                    `info`                     | The log level of this service; the HTTP stack is kept quieter. |
| RUST_LOG             |                 `EnvFilter` directives            |          |                                               | When set, overrides `LOG_LEVEL` entirely.      |
| CONFIGURATION_FOLDER |                     `path`                        |          | the platform config folder, e.g. `~/.config/catalog-mcp-server` | Folder holding `config.json`. |

#### Debugging with `RUST_LOG`

At `debug` the MCP SDK (`rmcp`) logs **every** JSON-RPC exchange in full: a `received request` line with
the call's arguments, and a `response message` line with the whole result. That holds for every tool,
for `tools/list` and for errors. It never includes HTTP headers, so the bearer token and the
`x-mia-acl-context` value cannot appear. The two sources can be switched independently:

| Goal | Setting |
| :--- | :--- |
| Production default: no payloads | `LOG_LEVEL=info` |
| Everything, including every request and response in full | `LOG_LEVEL=debug` |
| This service's debug events, without the SDK's payload dumps | `RUST_LOG=catalog_mcp_server=debug,rmcp=info,info` |
| Only the full requests and responses | `RUST_LOG=info,rmcp=debug` |

> [!WARNING]
> Payload logging copies tenant catalog data into the logs, and a single response can be tens of
> kilobytes. Use it as a temporary diagnostic setting, not as a production default.

### CLI options

| Flag                       | Required |                             Default                              | Description                     |
| :------------------------- | :------: | :--------------------------------------------------------------: | :------------------------------ |
| `--config-folder <FOLDER>` |          | `$CONFIGURATION_FOLDER`, else the platform config folder         | Folder holding `config.json`.   |
| `--version`                |          |                                                                  | Print the version and exit.     |

## Contributing

Contributions are welcome! Please check the [Contributing](./CONTRIBUTING.md) for guidelines on local development, standards, and other useful information.
