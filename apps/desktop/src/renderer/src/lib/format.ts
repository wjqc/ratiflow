/** 时间展示工具：相对时间与 HH:MM，统一各处时间格式。 */

export function relativeTime(iso: string | undefined | null): string {
  if (!iso) return '';
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return '';
  const diff = Date.now() - t;
  if (diff < 0) return formatDateTime(iso);
  const min = Math.floor(diff / 60000);
  if (min < 1) return '刚刚';
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  if (day === 1) return '昨天';
  if (day < 30) return `${day} 天前`;
  const d = new Date(t);
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}

/** HH:MM（当天）或 M月D日 HH:MM（跨天），用于时间线左列 */
export function timelineTime(iso: string | undefined | null): string {
  if (!iso) return '--:--';
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return '--:--';
  const d = new Date(t);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  return sameDay ? `${hh}:${mm}` : `${d.getMonth() + 1}/${d.getDate()} ${hh}:${mm}`;
}

export function formatDateTime(iso: string | undefined | null): string {
  if (!iso) return '—';
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const d = new Date(t);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 两个 ISO 时间的跨度（如 "3 小时 26 分钟"），无法计算时返回空串 */
export function durationBetween(fromIso?: string | null, toIso?: string | null): string {
  if (!fromIso || !toIso) return '';
  const from = Date.parse(fromIso);
  const to = Date.parse(toIso);
  if (Number.isNaN(from) || Number.isNaN(to) || to < from) return '';
  const min = Math.round((to - from) / 60000);
  if (min < 1) return '不到 1 分钟';
  if (min < 60) return `${min} 分钟`;
  const h = Math.floor(min / 60);
  const m = min % 60;
  if (h < 24) return m ? `${h} 小时 ${m} 分钟` : `${h} 小时`;
  const d = Math.floor(h / 24);
  return `${d} 天 ${h % 24} 小时`;
}
