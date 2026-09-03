// 行内两步确认 hook：Electron/自动化环境下 window.confirm 会被不可预期地自动放行
// （曾致项目误归档），全产品破坏性操作一律禁用 confirm，改用此模式——
// 第一次 request(id) 进入待确认态，3 秒内对同一 id 再 request 才执行；超时自动复位。
import { useEffect, useRef, useState } from 'react';

export function useTwoStepConfirm(): [
  string | null,
  (id: string, execute: () => void) => void,
] {
  const [pendingId, setPendingId] = useState<string | null>(null);
  const timer = useRef<number | null>(null);
  useEffect(() => {
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, []);
  const request = (id: string, execute: () => void) => {
    if (pendingId === id) {
      if (timer.current !== null) window.clearTimeout(timer.current);
      setPendingId(null);
      execute();
      return;
    }
    setPendingId(id);
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setPendingId(null), 3000);
  };
  return [pendingId, request];
}
