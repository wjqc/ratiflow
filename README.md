# Ratiflow · Governed AI Delivery

本地优先、可追溯、可审批、可回滚的 AI 软件交付工作台。Ratiflow 以项目知识库为上下文、Agent 为执行者、可配置工作流为治理机制，把需求、方案、开发、测试、部署与验证组织在同一个桌面工作区。内部包名、环境变量与数据目录继续沿用 `ratiflow` 标识，以保持升级兼容。

- **本地优先**：业务数据只写入本机 SQLite（单写者串行），秘密只进操作系统 Keychain。
- **治理内建**：Agent 的每一步工具调用都经过策略快照、审批与门禁校验，越界 fail-closed。
- **可观测**：模型调用的 Token 用量、缓存命中率、上下文压缩、审计与证据全程落库可回查。

## 功能

- **主工作台**：项目 → 六关（Requirements / Design / Development / Testing / Deployment / Verification）→ 文牒 → 部署 → 时间线的黄金流程；Agent 执行、审批中心与知识库检索在同一工作区。新建任务页为 ZCode 首页式大输入器——胶囊上下文行（工作区/关卡模板）、交付流程随所选模板自适应、聚焦弹提示菜单。
- **规范技能化接入**：任意外部 Markdown 目录（开发规范、团队手册）一键导入技能库——每个 .md 一个技能（名称按相对路径消歧）、draft 版本手动激活、秘密扫描 fail-closed、同内容重导幂等；新建任务输入器 `/` 选技能、`@` 指派 Agent（选中以胶囊呈现、创建时冻结版本）；run 技能注入三级优先（run 显式 > 任务默认 > 全局启用面），Agent 选路五级（任务显式 > 任务默认 > 项目绑定 > 全局绑定 > 内置通用），冻结面经 `agent.get` 可回查。
- **Agent Runtime**（ADR-033）：原生 function calling 主路径、流式输出与真取消、reasoning 状态加密持久化、checkpoint 暂停恢复；上下文超限自动压缩（保留冻结头与最近工具往返，system 段不变）。
- **执行沙箱**：Docker 一次性容器 / 内核沙箱（macOS Seatbelt、Linux Landlock）/ 本机只读白名单三档，写路径限定受管 worktree 与工件目录，默认禁网。
- **知识库与项目记忆**：来源扫描（固定排除 `.git`、`node_modules`、`target` 等）、分块、秘密扫描 fail-closed（命中不返回原文）、检索与上下文预算；项目记忆按 manifest 冻结 revision 注入且标注不可信边界；Run 成功后自动总结记忆候选进入待确认区，接受（可编辑）后激活。
- **MCP 服务器**：受控接入——注册、批准、schema digest 固定，活跃工具进入策略快照，写工具强制审批，撤销即失效；远程传输支持 SSE 与 Streamable HTTP（静态头脱敏）。技能市场源支持本机插件市场目录、远程 Git 仓库与远程清单 zip（安全解包、zip-slip 防护）。
- **外部集成**：GitLab 实例与 SSH 目标机在同一页配置；访问令牌/凭证直填即自动写入 Keychain（数据库只存引用 ID），GitLab 401 自动降级为 degraded，SSH 首次连接必须显式确认指纹。
- **设置中心**：行式布局；使用统计页提供总 Token / 缓存命中 Token / 缓存命中率三项指标与近 30 天每日趋势折线图（兼容 OpenAI 与 DeepSeek 两种缓存命中字段形状）；自动化（Automations）与治理总览独立成页。
- **备份与审计**：一键备份/恢复数据目录，审计导出全文不含任何秘密值。简单应用偏好存于数据目录 `settings.json` 单文件（原子替换、可手工编辑），治理状态仍留 SQLite。
- **可配置工作流与计划 DAG**：数据化工作流模板**默认可用**——自定义关卡数量/顺序/标题、每关多交付物 kind 与验收策略（acceptance）随版本冻结；新建任务可选模板；`RATIFLOW_WORKFLOW_TEMPLATE_V2=0` 可显式关闭。结构化计划（拓扑校验/环检测/确定性调度）、局部重规划（下游闭包重做、无关成功任务带证据复用）、任务级并行工作区（base HEAD 钉住、可归因可合并）；关卡跳过（skip）审批链、fast-track 六因素 shadow 评估、跨关返工统一失效登记与两步执行、六指标投影与 Triage 判重（FTS5 trigram 中文子串检索）。
- **自治与授权（灰度）**：Ask/Agent/Plan 模式与执行隔离正交；PlanGuard 执行前硬门禁（规划阶段只读，未知副作用 fail-closed）；AutonomyGrant 限范围限时授权；Goal 后台长任务默认停在人工放行。
- **Agent 团队与技能版本（灰度）**：Agent Team 版本化（role → profile version 选路，generic 回退带证据 / fail_closed 明确失败）；技能不可变版本生命周期（draft/active/deprecated/revoked）；Context Policy 服务端工具交集（客户端只可收紧）。
- **可观测与自动化（灰度）**：durable trace span 调用链、驾驶舱 read model（真实进度无估算值，checkpoint 断线重建）、usage 缓存/成本未知时诚实显示 unknown；自动化调度（receipt 幂等去重、misfire/overlap 策略、shadow 观察门槛与误报率自动回退）；中文 Slash 指令 preview → execute 命中同一审批链；私有技能仓库 pin commit SHA 导入（仅读取 Markdown，不执行任何代码）。

工作流模板域默认开启（`RATIFLOW_WORKFLOW_TEMPLATE_V2=0` 关闭）；其余灰度能力由 feature flag 门控（`RATIFLOW_PLAN_DAG` / `RATIFLOW_CONTEXT_POLICY_V2` / `RATIFLOW_AUTOMATIONS` 等，默认关闭）；开启方法与验收矩阵见仓库内实施文档。

## 架构

```text
apps/desktop        Electron main + preload + React renderer
crates/             Rust + Tokio（Cargo workspace，20 个 crate）
packages/protocol   从 contracts/rpc 生成的 TypeScript 协议类型
contracts/rpc       JSON-RPC 方法契约（TS/Rust 对齐的单一来源）
tests/              协议级黄金流程 + Agent 生命周期 + Electron E2E 场景
```

Electron main 与 Rust sidecar（`ratiflow-core app-server`）通过 JSON-RPC 2.0 over stdio 通信；renderer 无 Node/文件系统权限，业务数据只由 Rust core 写入。

Rust crates：
- 协议/存储：`protocol`（RPC 协议）、`store`（SQLite + 迁移 + 审计/备份）
- 业务域：`project`、`workitem`（六关与阶段状态机、交付物门禁、快照回滚）、`artifact`（文牒）、`knowledge`、`memory`、`context`（manifest 冻结、Context Policy）、`attachment`、`timeline`、`workflow`（工作流模板/计划 DAG/调度器/局部重规划/自动化）、`eventlog`、`provenance`、`evidence`
- 治理/执行：`policy`（工具策略、PlanGuard、自治授权）、`executor`（执行器与沙箱、任务工作区）、`agent`（Harness 运行时、Agent 生命周期、Team 选路、middleware registry）、`integrations`（GitLab/模型/SSH/MCP 适配器）
- 设置/应用：`settings`（键值设置、凭据 Keychain、模型/集成档案、备份/审计/诊断扩展）、`ratiflow-core`（JSON-RPC dispatch + sidecar 入口）

## 安全模型

- 凭据明文只写入 OS Keychain（macOS Keychain / 内存后端兜底），数据库仅存引用 ID，界面永不回显。
- 策略快照在 Run 启动时冻结：allowlist、审批要求、执行模式运行中不受设置变更影响。
- 高风险工具必须人工审批；审计导出与日志经秘密扫描，全程不含明文凭证。

## 下载安装

- **macOS**：从 [Releases](https://github.com/wjqc/ratiflow/releases) 下载 `.dmg`，拖入 Applications 即用。安装包未签名——首次打开请右键 →「打开」，或在「系统设置 → 隐私与安全性」中允许。
- 其他平台或源码运行见下方快速开始。

## 快速开始

要求：Rust toolchain（见 `rust-toolchain.toml`）、Node.js。

```bash
make install        # npm（Electron 走 npmmirror 镜像）
make build          # cargo release + desktop 构建
make run            # 启动桌面应用
```

可选集成（环境变量，未配置时诊断显示 not_ready 且以 fake 兜底，亦可在设置页内直填）：
`RATIFLOW_GITLAB_URL/TOKEN`、`RATIFLOW_MODEL_BASE_URL/API_KEY`、`RATIFLOW_SSH_HOST/USER`。

## 开发

```bash
make ci             # fmt + clippy + codegen + typecheck + 全部测试 + E2E
make codegen        # 修改 contracts/rpc/ratiflow.json 后重新生成 TS 类型
make test           # cargo test（20 个 crate）+ renderer vitest
make test-electron  # Playwright E2E 场景 A/D/E/F/G（需先 make build）
make test-contract  # 契约夹具 + 解码器 + 机密探针
make package        # 打包桌面应用（dir，未签名）
make package-dmg    # 打包 macOS dmg（未签名，随 Release 分发）
```

v2 数据迁移：`ratiflow-core migrate-v2 --from <旧data目录> --to <新目录>`（只读复制、行数校验、原目录可回退；v2 实现见 git 历史 `legacy/v2-go-spike`）。
