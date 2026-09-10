// 设置中心路由定义：SettingsRouteId 联合类型 + 分组导航元数据。
// 依 Ratiflow_设置中心功能与页面设计_v1.0.md §3 信息架构；禁止任意字符串分支。
import type { ComponentType } from 'react';
import {
  IconBook,
  IconCode,
  IconCpu,
  IconDb,
  IconDoc,
  IconFolder,
  IconGear,
  IconLayers,
  IconLink,
  IconServer,
  IconTarget,
  IconZap,
} from '../../components/Icons';

export type SettingsRouteId =
  | 'app-general'
  | 'projects'
  | 'workflow-templates'
  | 'knowledge-defaults'
  | 'memory'
  | 'models'
  | 'tools'
  | 'mcp'
  | 'skills'
  | 'automations'
  | 'governance'
  | 'execution'
  | 'agent-center'
  | 'integrations'
  | 'editors'
  | 'backup'
  | 'diagnostics';

/** real=真实 RPC 已接入；partial=部分真实部分待契约；pending=契约待 ZCode 提交。 */
export type SettingsAvailability = 'real' | 'partial' | 'pending';

export interface SettingsRouteMeta {
  id: SettingsRouteId;
  /** 页面设计文档编号，如 S00。 */
  code: string;
  name: string;
  availability: SettingsAvailability;
  icon: ComponentType<{ size?: number }>;
}

export interface SettingsNavGroup {
  label: string;
  items: SettingsRouteMeta[];
}

export const SETTINGS_NAV: SettingsNavGroup[] = [
  {
    label: '应用',
    items: [
      { id: 'app-general', code: 'S01', name: '常规', availability: 'real', icon: IconGear },
    ],
  },
  {
    label: '工作区',
    items: [
      { id: 'projects', code: 'S10', name: '项目与目录', availability: 'real', icon: IconFolder },
      { id: 'workflow-templates', code: 'S13', name: '工作流与关卡模板', availability: 'real', icon: IconLayers },
      { id: 'knowledge-defaults', code: 'S11', name: '知识库默认策略', availability: 'real', icon: IconBook },
      { id: 'memory', code: 'S12', name: '项目记忆', availability: 'real', icon: IconDb },
    ],
  },
  {
    label: 'Agent',
    items: [
      { id: 'models', code: 'S20', name: '模型与路由', availability: 'real', icon: IconCpu },
      { id: 'tools', code: 'S21', name: '工具与审批', availability: 'real', icon: IconCode },
      { id: 'mcp', code: 'S24', name: 'MCP 服务器', availability: 'real', icon: IconServer },
      { id: 'skills', code: 'S25', name: '技能', availability: 'real', icon: IconBook },
      { id: 'automations', code: 'S26', name: '自动化', availability: 'real', icon: IconZap },
      { id: 'governance', code: 'S60', name: '治理总览', availability: 'real', icon: IconTarget },
      { id: 'execution', code: 'S22', name: '执行与沙箱', availability: 'real', icon: IconZap },
      { id: 'agent-center', code: 'S23', name: 'Agent 中心', availability: 'real', icon: IconTarget },
    ],
  },
  {
    label: '集成',
    items: [
      { id: 'integrations', code: 'S33', name: '外部集成', availability: 'real', icon: IconLink },
      { id: 'editors', code: 'S32', name: '编辑器', availability: 'pending', icon: IconDoc },
    ],
  },
  {
    label: '数据与安全',
    items: [
      { id: 'backup', code: 'S41', name: '备份与恢复', availability: 'real', icon: IconDb },
    ],
  },
  {
    label: '支持',
    items: [
      { id: 'diagnostics', code: 'S50', name: '使用统计', availability: 'real', icon: IconZap },
    ],
  },
];

const ALL_SETTINGS_ITEMS = SETTINGS_NAV.flatMap((group) => group.items);

function settingsItems(ids: readonly SettingsRouteId[]): SettingsRouteMeta[] {
  return ids.map((id) => {
    const item = ALL_SETTINGS_ITEMS.find((candidate) => candidate.id === id);
    if (!item) throw new Error(`未知设置路由 ${id}`);
    return item;
  });
}

/** 普通用户的高频入口；保持短列表，避免把实现与治理概念全部摊开。 */
export const SETTINGS_PRIMARY_ITEMS = settingsItems([
  'app-general',
  'projects',
  'models',
]);

/** 低频、治理或排障能力按需展开；未完成的编辑器页不在导航中曝光。 */
export const SETTINGS_ADVANCED_ITEMS = settingsItems([
  'workflow-templates',
  'knowledge-defaults',
  'memory',
  'agent-center',
  'tools',
  'mcp',
  'skills',
  'automations',
  'governance',
  'execution',
  'backup',
  'diagnostics',
  'integrations',
]);

const ROUTE_MAP: ReadonlyMap<SettingsRouteId, SettingsRouteMeta> = new Map(
  ALL_SETTINGS_ITEMS.map((m) => [m.id, m]),
);

export function isSettingsRouteId(value: unknown): value is SettingsRouteId {
  return typeof value === 'string' && ROUTE_MAP.has(value as SettingsRouteId);
}

export function settingsRouteMeta(id: SettingsRouteId): SettingsRouteMeta {
  const meta = ROUTE_MAP.get(id);
  if (!meta) throw new Error(`未知设置路由 ${id}`);
  return meta;
}

/** 未知/缺失 section 的统一回退。 */
export const DEFAULT_SETTINGS_ROUTE: SettingsRouteId = 'app-general';
