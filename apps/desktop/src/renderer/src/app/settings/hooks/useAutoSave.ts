// 设置页自动保存调度：任何改动调用 schedule(delay) 后防抖合并持久化；
// 保存在途期间的新改动排队，结束后立即再存，保证不丢最新值。
// 页面把 persist() 传入：persist 内部读取最新 draft/revision 引用并发起 RPC，失败自行置错误，不抛出。
import { useEffect, useRef, useState } from 'react';

export function useAutoSave(persist: () => Promise<void>) {
  const [pending, setPending] = useState(false);
  const persistRef = useRef(persist);
  persistRef.current = persist;
  const busyRef = useRef(false);
  const queuedRef = useRef(false);
  const timerRef = useRef<number | null>(null);

  const flush = async () => {
    if (busyRef.current) {
      queuedRef.current = true;
      return;
    }
    busyRef.current = true;
    setPending(true);
    try {
      await persistRef.current();
    } catch {
      // persist 内部已处理错误；此处兜底避免未捕获拒绝。
    } finally {
      busyRef.current = false;
      if (queuedRef.current) {
        queuedRef.current = false;
        void flush();
      } else {
        setPending(false);
      }
    }
  };

  const schedule = (delay = 400) => {
    setPending(true);
    if (timerRef.current !== null) window.clearTimeout(timerRef.current);
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null;
      void flush();
    }, delay);
  };

  // 卸载时把防抖中的改动落盘（保存在途时由队列兜底）。
  useEffect(
    () => () => {
      if (timerRef.current !== null) {
        window.clearTimeout(timerRef.current);
        timerRef.current = null;
        if (!busyRef.current) {
          void persistRef.current();
        }
      }
    },
    [],
  );

  return { pending, schedule };
}
