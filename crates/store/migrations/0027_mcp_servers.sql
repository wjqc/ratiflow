-- 0027_mcp_servers: 受控 MCP ToolProvider（ADR-035 / Codex 能力差距方案 M6）。
-- server 注册与工具候选/激活分离；撤销即禁用（保留审计行）。
CREATE TABLE mcp_servers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    transport TEXT NOT NULL CHECK (transport IN ('stdio','https')),
    command TEXT NOT NULL DEFAULT '',
    args_json TEXT NOT NULL DEFAULT '[]',
    url TEXT NOT NULL DEFAULT '',
    server_info TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'candidate'
        CHECK (status IN ('candidate','active','revoked','probe_failed')),
    probe_error TEXT NOT NULL DEFAULT '',
    approved_by TEXT NOT NULL DEFAULT '',
    approved_at TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_mcp_servers_name ON mcp_servers(name);

CREATE TABLE mcp_server_tools (
    id TEXT PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    schema_json TEXT NOT NULL,
    schema_digest TEXT NOT NULL,
    read_only_hint INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'candidate'
        CHECK (status IN ('candidate','active','revoked','superseded')),
    created_at TEXT NOT NULL,
    UNIQUE(server_id, tool_name, schema_digest)
);
CREATE INDEX idx_mcp_tools_server ON mcp_server_tools(server_id, status);
