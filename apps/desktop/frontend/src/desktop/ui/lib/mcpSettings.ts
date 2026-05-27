import type {
  McpConfig,
  McpServerConfig,
  McpToolReport,
  McpTransport,
} from "../types";

export function inferMcpTransport(server: McpServerConfig): McpTransport {
  if (server.transport) return server.transport;
  return server.url ? "streamable_http" : "stdio";
}

export function normalizeMcpConfig(input: unknown): McpConfig {
  const source = input as
    | {
        mcp_servers?: unknown;
        mcpServers?: unknown;
        servers?: unknown;
      }
    | null
    | undefined;
  const raw = source?.mcp_servers ?? source?.mcpServers ?? source?.servers ?? {};
  const mcp_servers: Record<string, McpServerConfig> = {};
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    return { mcp_servers };
  }
  for (const [name, value] of Object.entries(raw as Record<string, unknown>)) {
    const server = value && typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : {};
    mcp_servers[name] = {
      name,
      transport: isMcpTransport(server.transport) ? server.transport : null,
      command: stringOrNull(server.command),
      args: Array.isArray(server.args) ? server.args.map(String) : [],
      env: objectToStringRecord(server.env),
      url: stringOrNull(server.url),
      headers: objectToStringRecord(server.headers),
      disabled: Boolean(server.disabled),
    };
  }
  return { mcp_servers };
}

export function toCamelMcpConfig(config: McpConfig | unknown) {
  const normalized = normalizeMcpConfig(config);
  const mcpServers: Record<string, unknown> = {};
  for (const [name, server] of Object.entries(normalized.mcp_servers)) {
    const transport = server.transport ?? inferMcpTransport(server);
    const item: Record<string, unknown> = {
      transport,
      disabled: server.disabled,
    };
    if (transport === "stdio") {
      item.command = server.command ?? "";
      item.args = server.args ?? [];
      if (Object.keys(server.env ?? {}).length > 0) item.env = server.env;
    } else {
      item.url = server.url ?? "";
      if (Object.keys(server.headers ?? {}).length > 0) item.headers = server.headers;
    }
    mcpServers[name] = item;
  }
  return { mcpServers };
}

export function parseMcpJson(text: string): McpConfig {
  if (!text.trim()) return { mcp_servers: {} };
  return normalizeMcpConfig(JSON.parse(text));
}

export function indexMcpToolReports(
  reports: McpToolReport[] | null | undefined
): Record<string, McpToolReport> {
  const out: Record<string, McpToolReport> = {};
  for (const report of reports ?? []) {
    if (!report || typeof report.server_name !== "string") continue;
    out[report.server_name] = {
      ...report,
      tools: Array.isArray(report.tools) ? report.tools : [],
      error: report.error ?? null,
    };
  }
  return out;
}

export function objectToStringRecord(input: unknown): Record<string, string> {
  if (!input || typeof input !== "object" || Array.isArray(input)) return {};
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(input as Record<string, unknown>)) {
    if (v == null) continue;
    out[k] = String(v);
  }
  return out;
}

function isMcpTransport(input: unknown): input is McpTransport {
  return input === "stdio" || input === "streamable_http" || input === "sse";
}

function stringOrNull(input: unknown): string | null {
  return typeof input === "string" ? input : null;
}
