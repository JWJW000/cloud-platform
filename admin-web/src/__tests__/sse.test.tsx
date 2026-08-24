// SSE 状态测试（V5 第 5.7 节）：连接中/已连接/重连中/已断开，绝不写死「在线」。
import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useSse } from "../hooks/useSse";

class FakeEventSource {
  static instances: FakeEventSource[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((e: MessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  listeners: Record<string, ((e: MessageEvent) => void)[]> = {};
  closed = false;

  constructor(public url: string, public opts?: { withCredentials?: boolean }) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(name: string, fn: (e: MessageEvent) => void) {
    (this.listeners[name] ??= []).push(fn);
  }

  emitOpen() {
    this.onopen?.();
  }

  emitMessage(data: string) {
    this.onmessage?.({ data } as MessageEvent);
  }

  emitNamed(name: string, data: string) {
    for (const fn of this.listeners[name] ?? []) fn({ data } as MessageEvent);
  }

  emitError() {
    this.onerror?.();
  }

  close() {
    this.closed = true;
  }
}

vi.stubGlobal("EventSource", FakeEventSource);

afterEach(() => {
  FakeEventSource.instances = [];
  vi.useRealTimers();
});

describe("useSse", () => {
  it("初始为已断开；启用后进入连接中", () => {
    const { result } = renderHook(() => useSse(true));
    expect(result.current.state).toBe("connecting");
  });

  it("打开连接后变为已连接，收到事件更新 lastEventAt", async () => {
    const { result } = renderHook(() => useSse(true));
    const es = FakeEventSource.instances[0];
    await act(async () => {
      es.emitOpen();
    });
    expect(result.current.state).toBe("connected");
    await act(async () => {
      es.emitMessage(JSON.stringify({ type: "ping" }));
    });
    expect(result.current.lastEventAt).not.toBeNull();
  });

  it("断开后进入重连中并累计重连次数", async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSse(true));
    const es = FakeEventSource.instances[0];
    await act(async () => {
      es.emitError();
    });
    expect(result.current.state).toBe("reconnecting");
    expect(result.current.reconnectCount).toBe(1);
  });

  it("禁用后回到已断开", () => {
    const { result, rerender } = renderHook(({ enabled }) => useSse(enabled), {
      initialProps: { enabled: true },
    });
    rerender({ enabled: false });
    expect(result.current.state).toBe("disconnected");
  });
});
