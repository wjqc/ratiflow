// F2/F3 契约待提交页面（§5）：结构与等待项真实可读，不伪造"已配置"、不产生写入。
import { SettingsPageHeader } from './components/SettingsPageHeader';
import { SettingsSection } from './components/SettingsSection';
import { ContractPending } from './components/ContractPending';

interface PendingDef {
  title: string;
  scope: '全局' | '项目' | '环境' | '本地';
  description: string;
  features: string[];
  awaiting: string[];
  fallback: string;
}

const DEFS: Record<string, PendingDef> = {
  'app-general': {
    title: '常规',
    scope: '全局',
    description: '应用级偏好：启动恢复、默认项目、语言与时间格式、下载目录、遥测（默认关闭）。',
    features: [
      '启动时恢复上次项目与页面',
      '默认项目与默认落地目录',
      '语言（中文/英文）与时间格式（相对/绝对）',
      '遥测开关（默认关闭，仅本地匿名诊断计数）',
    ],
    awaiting: ['settings.get', 'settings.update'],
    fallback: '当前版本为默认行为：恢复上次项目与页面已在本地生效（localStorage），其余为固定默认值。',
  },
  appearance: {
    title: '外观',
    scope: '全局',
    description: '主题、密度、代码字体与动效偏好；遵循系统对比度与 prefers-reduced-motion。',
    features: [
      '主题：浅色（MVP）/ 深色 / 跟随系统',
      '界面密度：舒适 / 紧凑',
      '代码字体与字号',
      '动效减弱开关（与系统 prefers-reduced-motion 联动）',
      '侧栏宽度与默认折叠',
    ],
    awaiting: ['settings.get', 'settings.update（外观域）'],
    fallback: 'MVP 仅浅色主题；系统级对比度与动效偏好已被样式层遵循。',
  },
  'knowledge-defaults': {
    title: '知识库默认策略',
    scope: '全局',
    description: '新项目的知识库默认摄入与检索策略；项目级覆盖在项目知识库页管理。',
    features: [
      '默认允许来源与默认忽略 glob',
      '默认排除文档类来源开关',
      '单文件大小上限、分块大小与重叠',
      '秘密扫描策略（命中即拒 / 警告）',
      '检索边界与 context 预算上限默认值',
    ],
    awaiting: ['knowledge.settings.get', 'knowledge.settings.update'],
    fallback: '项目级来源请在项目知识库页管理；全局默认值当前为内置策略。',
  },
  models: {
    title: '模型与路由',
    scope: '全局',
    description: 'Provider / 模型档案与按阶段路由规则；秘密值仅存 keychain 引用，永远不入库。',
    features: [
      'Provider 列表与连接状态（baseUrl / apiKeyRef）',
      '新增 Provider 向导：连接测试（真实 HTTP）→ 模型同步 → 保存',
      '模型档案：上下文长度、JSON 能力、工具调用能力',
      '按阶段路由规则与 fallback 顺序',
      '每阶段预算上限与分步测试',
    ],
    awaiting: ['modelProfile.list/get/create/update/delete', 'modelProfile.testConnection', 'modelRoute.get/update'],
    fallback: '当前经 SIXGATES_MODEL_* 环境变量启动装配；装配状态见「使用统计」。',
  },
  tools: {
    title: '工具与审批',
    scope: '全局',
    description: 'Agent 工具开关、风险等级与审批策略；含每工具风险与最近使用排障信息。',
    features: [
      '工具表：开关 / 风险等级 / 审批要求 / 网络访问',
      '超时、重试与输出截断策略',
      '工具详情抽屉：schema、风险说明、审批规则、最近使用',
      '高风险工具变更二次确认',
    ],
    awaiting: ['toolPolicy.list', 'toolPolicy.effective', 'toolPolicy.update'],
    fallback: '当前为内置快照策略（无动态写入）；审批卡在审批中心处理。',
  },
  execution: {
    title: '执行与沙箱',
    scope: '环境',
    description: 'Agent 命令执行模式与沙箱资源限制；切换前自动运行环境自检。',
    features: [
      '执行模式：本机直接 / Docker 容器',
      'Docker 可用性检测与版本展示',
      'CPU / 内存 / 进程数 / 网络白名单限制',
      '挂载策略与工作目录读写规则',
      '模式切换前的环境自检结果',
    ],
    awaiting: ['executionProfile.list', 'executionProfile.create', 'executionProfile.update', 'executor.settings.get', 'executor.settings.update', 'executor.check'],
    fallback: '当前模式由启动环境检测决定，实际状态见「使用统计」本地执行器项。',
  },
  editors: {
    title: '编辑器',
    scope: '本地',
    description: '检测本机编辑器与"打开方式"命令模板；可选页，不影响整体 ready 状态。',
    features: [
      '已安装编辑器检测（VS Code / Cursor 等）',
      '默认打开命令与命令模板',
      'Ratiflow 扩展状态',
    ],
    awaiting: ['editor.detect（S32 可选，未进契约——非发布阻塞）'],
    fallback: '可在系统文件管理器中直接打开项目目录。',
  },
};

export function PendingSettingsPage({ id }: { id: keyof typeof DEFS }) {
  const def = DEFS[id] ?? {
    title: '页面未接线',
    scope: '本地' as const,
    description: `路由 ${String(id)} 尚未映射到实现页面（路由表与页面壳不一致）。`,
    features: [],
    awaiting: [],
    fallback: '请检查 SettingsShell 的分支注册。',
  };
  return (
    <div className="sg-set-page">
      <SettingsPageHeader title={def.title} scope={def.scope} description={def.description} />
      <SettingsSection title="功能规划与契约状态">
        <ContractPending features={def.features} awaiting={def.awaiting} fallback={def.fallback} />
      </SettingsSection>
    </div>
  );
}
