-- 0058_mcp_remote_transports: 远程 MCP 两种传输类型（SSE / Streamable HTTP）。
-- 0027 的 transport CHECK 只允许 ('stdio','https')——'https' 从未被写入（server_add
-- 恒 stdio）；本迁移把合法值改为 ('stdio','sse','streamable-http') 并新增
-- headers_json（静态头注册时冻结，值不进读模型）。SQLite 不能改 CHECK：重建表。
-- FK 关闭窗口内重建（mcp_server_tools 经 ON DELETE CASCADE 引用本表，行原样保留）。
CREATE TABLE mcp_servers_v2 (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    transport TEXT NOT NULL CHECK (transport IN ('stdio','sse','streamable-http')),
    command TEXT NOT NULL DEFAULT '',
    args_json TEXT NOT NULL DEFAULT '[]',
    url TEXT NOT NULL DEFAULT '',
    headers_json TEXT NOT NULL DEFAULT '[]',
    server_info TEXT NOT NULL DEFAULT '',
    enabled INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'candidate'
        CHECK (status IN ('candidate','active','revoked','probe_failed')),
    probe_error TEXT NOT NULL DEFAULT '',
    approved_by TEXT NOT NULL DEFAULT '',
    approved_at TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);
INSERT INTO mcp_servers_v2(id, name, transport, command, args_json, url, headers_json,
    server_info, enabled, status, probe_error, approved_by, approved_at, created_at)
SELECT id, name,
       CASE transport WHEN 'https' THEN 'streamable-http' ELSE transport END,
       command, args_json, url, '[]',
       server_info, enabled, status, probe_error, approved_by, approved_at, created_at
FROM mcp_servers;
DROP TABLE mcp_servers;
ALTER TABLE mcp_servers_v2 RENAME TO mcp_servers;
CREATE UNIQUE INDEX idx_mcp_servers_name ON mcp_servers(name);
