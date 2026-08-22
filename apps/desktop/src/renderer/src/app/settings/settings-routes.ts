// 设置中心路由定义：SettingsRouteId 联合类型 + 分组导航元数据。
// 依 SixGates_设置中心功能与页面设计_v1.0.md §3 信息架构；禁止任意字符串分支。
import type { ComponentType } from 'react';
import {
  IconBook,
  IconCloud,
  IconCode,
  IconCpu,
  IconDb,
  IconDoc,
  IconDownload,
  IconFolder,
  IconGear,
  IconLayers,
  IconLink,
  IconServer,
  IconShield,
  IconTarget,
  IconZap,
} from '../../components/Icons';

export type SettingsRouteId =
  | 'overview'
  | 'app-general'
  | 'appearance'
  | 'updates'
  | 'projects'
  | 'knowledge-defaults'
  | 'models'
  | 'tools'
  | 'execution'
  | 'gitlab'
  | 'ssh'
  | 'editors'
  | 'credentials'
  | 'backup'
  | 'audit'
  | 'diagnostics'
  | 'logs';

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
    label: '概览',
    items: [
      { id: 'overview', code: 'S00', name: '设置概览', availability: 'real', icon: IconTarget },
    ],
  },
  {
    label: '应用',
    items: [
      { id: 'app-general', code: 'S01', name: '常规', availability: 'pending', icon: IconGear },
      { id: 'appearance', code: 'S02', name: '外观', availability: 'pending', icon: IconLayers },
      { id: 'updates', code: 'S03', name: '更新与关于', availability: 'partial', icon: IconDownload },
    ],
  },
  {
    label: '工作区',
    items: [
      { id: 'projects', code: 'S10', name: '项目与目录', availability: 'real', icon: IconFolder },
      { id: 'knowledge-defaults', code: 'S11', name: '知识库默认策略', availability: 'pending', icon: IconBook },
    ],
  },
  {
    label: 'Agent',
    items: [
      { id: 'models', code: 'S20', name: '模型与路由', availability: 'pending', icon: IconCpu },
      { id: 'tools', code: 'S21', name: '工具与审批', availability: 'pending', icon: IconCode },
      { id: 'execution', code: 'S22', name: '执行与沙箱', availability: 'pending', icon: IconZap },
    ],
  },
  {
    label: '集成',
    items: [
      { id: 'gitlab', code: 'S30', name: 'GitLab', availability: 'pending', icon: IconLink },
      { id: 'ssh', code: 'S31', name: 'SSH 目标机', availability: 'pending', icon: IconServer },
      { id: 'editors', code: 'S32', name: '编辑器', availability: 'pending', icon: IconDoc },
    ],
  },
  {
    label: '数据与安全',
    items: [
      { id: 'credentials', code: 'S40', name: '凭据引用', availability: 'pending', icon: IconShield },
      { id: 'backup', code: 'S41', name: '备份与恢复', availability: 'partial', icon: IconDb },
      { id: 'audit', code: 'S42', name: '审计日志', availability: 'real', icon: IconDoc },
    ],
  },
  {
    label: '支持',
    items: [
      { id: 'diagnostics', code: 'S50', name: '运行与集成诊断', availability: 'real', icon: IconZap },
      { id: 'logs', code: 'S51', name: '日志与故障报告', availability: 'partial', icon: IconCloud },
    ],
  },
];

const ROUTE_MAP: ReadonlyMap<SettingsRouteId, SettingsRouteMeta> = new Map(
  SETTINGS_NAV.flatMap((g) => g.items).map((m) => [m.id, m]),
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
export const DEFAULT_SETTINGS_ROUTE: SettingsRouteId = 'overview';
