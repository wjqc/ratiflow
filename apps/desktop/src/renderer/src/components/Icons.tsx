/**
 * Ratiflow 内联 SVG 图标集（单色线性，currentColor）。
 * 不引入图标依赖，统一 16px 网格、1.6 描边。
 */
import type { SVGProps } from 'react';

type IconProps = SVGProps<SVGSVGElement> & { size?: number };

function base(size: number) {
  return {
    width: size,
    height: size,
    viewBox: '0 0 16 16',
    fill: 'none',
    stroke: 'currentColor',
    strokeWidth: 1.6,
    strokeLinecap: 'round' as const,
    strokeLinejoin: 'round' as const,
    'aria-hidden': true as const,
    focusable: false as const,
  };
}

export const IconLogo = ({ size = 20, ...rest }: IconProps) => (
  <svg width={size} height={size} viewBox="0 0 20 20" fill="none" aria-hidden focusable={false} {...rest}>
    <rect x="2" y="3" width="10" height="13" rx="2.2" fill="#1769e8" opacity="0.28" />
    <rect x="5" y="4" width="10" height="13" rx="2.2" fill="#1769e8" opacity="0.56" />
    <rect x="8" y="5.5" width="10" height="11.5" rx="2.2" fill="#1769e8" />
    <path d="M10.4 11.1l2.1 2 3.4-4" stroke="#fff" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
  </svg>
);

export const IconHome = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M2.5 6.5L8 2l5.5 4.5V13a1 1 0 0 1-1 1H9.5v-3.5h-3V14H3.5a1 1 0 0 1-1-1V6.5z" />
  </svg>
);

export const IconPlus = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M8 3v10M3 8h10" />
  </svg>
);

export const IconSearch = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <circle cx="7" cy="7" r="4.2" />
    <path d="M10.2 10.2L14 14" />
  </svg>
);

export const IconFolder = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 2h5A1.5 1.5 0 0 1 14.5 6.5v5A1.5 1.5 0 0 1 13 13H3a1.5 1.5 0 0 1-1.5-1.5v-7z" />
  </svg>
);

export const IconChevronDown = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M4 6l4 4 4-4" />
  </svg>
);

export const IconChevronRight = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M6 4l4 4-4 4" />
  </svg>
);

export const IconBook = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M2.5 3.5A1.5 1.5 0 0 1 4 2h9.5v11.5H4a1.5 1.5 0 0 0-1.5 1.5v-11z" />
    <path d="M13.5 13.5H4a1.5 1.5 0 0 0-1.5 1.5" />
  </svg>
);

export const IconInfo = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <circle cx="8" cy="8" r="6" />
    <path d="M8 7.2V11M8 5.2v.1" />
  </svg>
);

export const IconGear = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <circle cx="8" cy="8" r="2.2" />
    <path d="M8 1.8v1.7M8 12.5v1.7M1.8 8h1.7M12.5 8h1.7M3.6 3.6l1.2 1.2M11.2 11.2l1.2 1.2M12.4 3.6l-1.2 1.2M4.8 11.2l-1.2 1.2" />
  </svg>
);

export const IconShield = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M8 1.8l5.2 2v4.4c0 3.2-2.2 5.3-5.2 6.3-3-1-5.2-3.1-5.2-6.3V3.8l5.2-2z" />
  </svg>
);

export const IconSend = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M8 13V3M4 7l4-4 4 4" />
  </svg>
);

export const IconPaperclip = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M10.5 4.5l-5 5a2 2 0 1 0 2.8 2.8l5-5a3.4 3.4 0 1 0-4.8-4.8l-5 5" />
  </svg>
);

export const IconDoc = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M4 1.8h5.5L13 5.3V14a.9.9 0 0 1-.9.9H4a.9.9 0 0 1-.9-.9V2.7a.9.9 0 0 1 .9-.9z" />
    <path d="M9.3 2v3.5H13M5.8 8.4h4.4M5.8 11h4.4" />
  </svg>
);

export const IconIssue = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <circle cx="8" cy="8" r="6" />
    <circle cx="8" cy="8" r="1.6" fill="currentColor" stroke="none" />
  </svg>
);

export const IconImage = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
    <circle cx="5.6" cy="6.2" r="1.1" />
    <path d="M2.5 11.5l3-3 2.5 2.5 3-3 2.5 2.5" />
  </svg>
);

export const IconClock = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <circle cx="8" cy="8" r="6" />
    <path d="M8 4.6V8l2.3 1.5" />
  </svg>
);

export const IconTarget = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <circle cx="8" cy="8" r="6" />
    <circle cx="8" cy="8" r="2.4" />
  </svg>
);

export const IconCheck = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M3 8.5l3.2 3.2L13 5" />
  </svg>
);

export const IconX = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M4 4l8 8M12 4l-8 8" />
  </svg>
);

export const IconAlert = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M8 1.8L14.5 13h-13L8 1.8z" />
    <path d="M8 6v3.4M8 11.2v.1" />
  </svg>
);

export const IconRefresh = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9" />
    <path d="M13.7 1.8v2.6h-2.6" />
  </svg>
);

export const IconEdit = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M9.5 3.2l3.3 3.3L6 13.3l-3.7.4.4-3.7 6.8-6.8z" />
  </svg>
);

export const IconMore = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <circle cx="3.5" cy="8" r="1" fill="currentColor" stroke="none" />
    <circle cx="8" cy="8" r="1" fill="currentColor" stroke="none" />
    <circle cx="12.5" cy="8" r="1" fill="currentColor" stroke="none" />
  </svg>
);

export const IconLink = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M6.5 9.5a3 3 0 0 0 4.2 0l2-2a3 3 0 1 0-4.2-4.2l-1 1" />
    <path d="M9.5 6.5a3 3 0 0 0-4.2 0l-2 2a3 3 0 1 0 4.2 4.2l1-1" />
  </svg>
);

export const IconUser = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <circle cx="8" cy="5.4" r="2.6" />
    <path d="M2.8 13.8a5.6 5.6 0 0 1 10.4 0" />
  </svg>
);

export const IconDb = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <ellipse cx="8" cy="3.6" rx="5.2" ry="2" />
    <path d="M2.8 3.6v8.8c0 1.1 2.3 2 5.2 2s5.2-.9 5.2-2V3.6M2.8 8c0 1.1 2.3 2 5.2 2s5.2-.9 5.2-2" />
  </svg>
);

export const IconCloud = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M4.5 12.5a2.8 2.8 0 0 1-.4-5.6 4 4 0 0 1 7.7-1 3 3 0 0 1-.3 6.6h-7z" />
  </svg>
);

export const IconServer = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <rect x="2.2" y="2.4" width="11.6" height="4.6" rx="1" />
    <rect x="2.2" y="9" width="11.6" height="4.6" rx="1" />
    <path d="M4.8 4.7h.1M4.8 11.3h.1" />
  </svg>
);

export const IconCode = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M5.5 4.5L2 8l3.5 3.5M10.5 4.5L14 8l-3.5 3.5" />
  </svg>
);

export const IconCpu = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <rect x="4" y="4" width="8" height="8" rx="1" />
    <path d="M6.5 1.8V4M9.5 1.8V4M6.5 12v2.2M9.5 12v2.2M1.8 6.5H4M1.8 9.5H4M12 6.5h2.2M12 9.5h2.2" />
  </svg>
);

export const IconLayers = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M8 1.8l6 3-6 3-6-3 6-3z" />
    <path d="M2 8.5l6 3 6-3M2 11.8l6 3 6-3" />
  </svg>
);

/** 侧栏开合（左侧面板 + 竖分隔线）。 */
export const IconPanelLeft = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <rect x="1.8" y="2.3" width="12.4" height="11.4" rx="1.6" />
    <path d="M6 2.5v11" />
  </svg>
);

export const IconInbox = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M2.5 9.5h3l1 1.5h3l1-1.5h3" />
    <path d="M3.2 3h9.6l1.2 6.5v3a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1v-3L3.2 3z" />
  </svg>
);

export const IconZap = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M8.8 1.8L3.5 9h3.6l-1 5.2L11.4 7H7.8l1-5.2z" />
  </svg>
);

export const IconArrowLeft = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M10 3L5 8l5 5M5.5 8H14" />
  </svg>
);

export const IconPlay = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M4.5 2.8l8 5.2-8 5.2V2.8z" />
  </svg>
);

export const IconRocket = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M8 1.8c2.8 1.2 4 4 4 7l-2.5 2.5h-3L4 8.8c0-3 1.2-5.8 4-7z" />
    <circle cx="8" cy="6.5" r="1.2" />
    <path d="M6.5 11.3L5 14.5M9.5 11.3l1.5 3.2" />
  </svg>
);

export const IconFlask = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M6.5 1.8h3M7 1.8v4l-3.6 6.4a1.4 1.4 0 0 0 1.2 2.1h6.8a1.4 1.4 0 0 0 1.2-2.1L9 5.8v-4" />
    <path d="M4.8 10h6.4" />
  </svg>
);

export const IconDownload = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M8 2v8M4.8 7.2L8 10.4l3.2-3.2M2.5 13.5h11" />
  </svg>
);

export const IconText = ({ size = 16, ...rest }: IconProps) => (
  <svg {...base(size)} {...rest}>
    <path d="M3 4.5V3h10v1.5M8 3v10M5.8 13h4.4" />
  </svg>
);
