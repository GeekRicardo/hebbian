import { indexMcpToolReports, normalizeMcpConfig } from "./mcpSettings";

function expectEqual(name: string, actual: unknown, expected: unknown) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    throw new Error(`${name}: expected ${e}, got ${a}`);
  }
}

expectEqual(
  "normalizes null mcp config to an empty server record",
  normalizeMcpConfig(null),
  { mcp_servers: {} }
);

expectEqual(
  "indexes discovered tool reports by server name",
  Object.keys(
    indexMcpToolReports([
      {
        server_name: "filesystem",
        transport: "stdio",
        disabled: false,
        tools: [
          {
            server_name: "filesystem",
            name: "read_file",
            runtime_name: "Mcp__filesystem__read_file",
            description: "Read a file",
            input_schema: { type: "object" },
          },
        ],
        error: null,
      },
    ])
  ),
  ["filesystem"]
);
