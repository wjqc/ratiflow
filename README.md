# 通关 SixGates · 桌面版

本地优先的 AI 软件交付工作台：以项目知识库为上下文、Agent 为执行者、六关门禁为治理机制，把需求 → 方案 → 开发 → 测试 → 部署 → 验证组织在同一个桌面工作区。

- **本地优先**：业务数据只写入本机 SQLite（单写者串行），秘密只进操作系统 Keychain。
- **治理内建**：Agent 的每一步工具调用都经过策略快照、审批与门禁校验，越界 fail-closed。
- **可观测**：模型调用的 Token 用量、缓存命中率、上下文压缩、审计与证据全程落库可回查。

## 功能

- **主工作台**：项目 → 六关（Requirements / Design / Development / Testing / Deployment / Verification）→ 文牒 → 部署 → 时间线的黄金流程；Agent 执行、审批中心与知识库检索在同一工作区。
- **Agent Runtime**（ADR-033）：原生 function calling 主路径、流式输出与真取消、reasoning 状态加密持久化、checkpoint 暂停恢复；上下文超限自动压缩（保留冻结头与最近工具往返，system 段不变）。
- **执行沙箱**：Docker 一次性容器 / 内核沙箱（macOS Seatbelt、Linux Landlock）/ 本机只读白名单三档，写路径限定受管 worktree 与工件目录，默认禁网。
- **知识库与项目记忆**：来源扫描（固定排除 `.git`、`node_modules`、`target` 等）、分块、秘密扫描 fail-closed（命中不返回原文）、检索与上下文预算；项目记忆按 manifest 冻结 revision 注入且标注不可信边界。
- **MCP 服务器**：受控接入——注册、批准、schema digest 固定，活跃工具进入策略快照，写工具强制审批，撤销即失效。
- **外部集成**：GitLab 实例与 SSH 目标机在同一页配置；访问令牌/凭证直填即自动写入 Keychain（数据库只存引用 ID），GitLab 401 自动降级为 degraded，SSH 首次连接必须显式确认指纹。
- **设置中心**：行式布局；使用统计页提供总 Token / 缓存命中 Token / 缓存命中率三项指标与近 30 天每日趋势折线图（兼容 OpenAI 与 DeepSeek 两种缓存命中字段形状）。
- **备份与审计**：一键备份/恢复数据目录，审计导出全文不含任何秘密值。

## 架构

```text
apps/desktop        Electron main + preload + React renderer
crates/             Rust + Tokio（Cargo workspace，20 个 crate）
packages/protocol   从 contracts/rpc 生成的 TypeScript 协议类型
contracts/rpc       JSON-RPC 方法契约（TS/Rust 对齐的单一来源）
tests/              协议级黄金流程 + Agent 生命周期 + Electron E2E 场景
```

Electron main 与 Rust sidecar（`sixgates-core app-server`）通过 JSON-RPC 2.0 over stdio 通信；renderer 无 Node/文件系统权限，业务数据只由 Rust core 写入。

Rust crates：
- 协议/存储：`protocol`（RPC 协议）、`store`（SQLite + 迁移 + 审计/备份）
- 业务域：`project`、`workitem`（六关与阶段状态机）、`artifact`（文牒）、`knowledge`、`memory`、`context`、`attachment`、`timeline`、`workflow`、`eventlog`、`provenance`、`evidence`
- 治理/执行：`policy`（工具策略）、`executor`（执行器与沙箱）、`agent`（Harness 运行时与 Agent 生命周期）、`integrations`（GitLab/模型/SSH/MCP 适配器）
- 设置/应用：`settings`（键值设置、凭据 Keychain、模型/集成档案、备份/审计/诊断扩展）、`sixgates-core`（JSON-RPC dispatch + sidecar 入口）

## 安全模型

- 凭据明文只写入 OS Keychain（macOS Keychain / 内存后端兜底），数据库仅存引用 ID，界面永不回显。
- 策略快照在 Run 启动时冻结：allowlist、审批要求、执行模式运行中不受设置变更影响。
- 高风险工具必须人工审批；审计导出与日志经秘密扫描，全程不含明文凭证。

## 快速开始

要求：Rust toolchain（见 `rust-toolchain.toml`）、Node.js。

```bash
make install        # npm（Electron 走 npmmirror 镜像）
make build          # cargo release + desktop 构建
make run            # 启动桌面应用
```

可选集成（环境变量，未配置时诊断显示 not_ready 且以 fake 兜底，亦可在设置页内直填）：
`SIXGATES_GITLAB_URL/TOKEN`、`SIXGATES_MODEL_BASE_URL/API_KEY`、`SIXGATES_SSH_HOST/USER`。

## 开发

```bash
make ci             # fmt + clippy + codegen + typecheck + 全部测试 + E2E
make codegen        # 修改 contracts/rpc/sixgates.json 后重新生成 TS 类型
make test           # cargo test（20 个 crate）+ renderer vitest
make test-electron  # Playwright E2E 场景 A/D/E/F/G（需先 make build）
make test-contract  # 契约夹具 + 解码器 + 机密探针
make package        # 打包桌面应用（dir，未签名）
```

v2 数据迁移：`sixgates-core migrate-v2 --from <旧data目录> --to <新目录>`（只读复制、行数校验、原目录可回退；v2 实现见 git 历史 `legacy/v2-go-spike`）。
