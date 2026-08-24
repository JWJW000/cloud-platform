// SSE 实时状态 hook（V5 第 5.7 节）：反映真实连接生命周期。
//
// 状态机：connecting → connected → reconnecting（自动重连）→ disconnected（手动关闭）。
// 绝不写死「在线」：徽章只反映 EventSource 的真实 readyState 与重连结果。

import { useEffect, useRef, useState } from "react";

export type SseState = "connecting" | "connected" | "reconnecting" | "disconnected";

export interface SseSnapshot {
  state: SseState;
  /** 最近一次收到事件的时间（未收到为 null）。 */
  lastEventAt: string | null;
  /** 重连次数（自动重连累计）。 */
  reconnectCount: number;
}

const RECONNECT_DELAY_MS = 3000;

/**
 * 订阅 /api/events 实时事件流。
 *
 * @param onEvent 收到事件的回调（传入事件名与数据）。
 * @param enabled 是否启用（登录后才启用）。
 */
export function useSse(
  enabled: boolean,
  onEvent?: (event: string, data: unknown) => void,
): SseSnapshot {
  const [snapshot, setSnapshot] = useState<SseSnapshot>({
    state: "disconnected",
    lastEventAt: null,
    reconnectCount: 0,
  });
  const onEventRef = useRef(onEvent);
  onEventRef.current = onEvent;

  useEffect(() => {
    if (!enabled) {
      setSnapshot({ state: "disconnected", lastEventAt: null, reconnectCount: 0 });
      return;
    }

    let closed = false;
    let es: EventSource | null = null;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

    const setState = (state: SseState, reconnectCount: number) => {
      if (closed) return;
      setSnapshot((prev) => ({ ...prev, state, reconnectCount }));
    };

    const connect = () => {
      if (closed) return;
      setState("connecting", 0);
      es = new EventSource("/api/events", { withCredentials: true });

      es.onopen = () => {
        if (closed) return;
        setSnapshot((prev) => ({ ...prev, state: "connected" }));
      };

      es.onmessage = (e) => {
        if (closed) return;
        setSnapshot((prev) => ({
          ...prev,
          state: "connected",
          lastEventAt: new Date().toISOString(),
        }));
        try {
          const parsed = JSON.parse(e.data) as unknown;
          onEventRef.current?.("message", parsed);
        } catch {
          onEventRef.current?.("message", e.data);
        }
      };

      // 命名事件（SSE 规范：event: 节点变更\ndata: {...}）
      const namedHandler = (eventName: string) => (e: MessageEvent) => {
        if (closed) return;
        setSnapshot((prev) => ({ ...prev, state: "connected", lastEventAt: new Date().toISOString() }));
        try {
          const parsed = JSON.parse(e.data) as unknown;
          onEventRef.current?.(eventName, parsed);
        } catch {
          onEventRef.current?.(eventName, e.data);
        }
      };
      const common = ["节点变更", "任务变更", "批次变更", "告警", "账号变更", "代理变更"];
      for (const name of common) {
        es.addEventListener(name, namedHandler(name));
      }

      es.onerror = () => {
        if (closed) return;
        // 关闭旧连接并按固定间隔自动重连
        es?.close();
        setSnapshot((prev) => ({
          ...prev,
          state: "reconnecting",
          reconnectCount: prev.reconnectCount + 1,
        }));
        reconnectTimer = setTimeout(connect, RECONNECT_DELAY_MS);
      };
    };

    connect();

    return () => {
      closed = true;
      es?.close();
      if (reconnectTimer) clearTimeout(reconnectTimer);
    };
  }, [enabled]);

  return snapshot;
}
