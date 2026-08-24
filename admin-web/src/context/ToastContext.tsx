// 全局操作反馈（V5 第 5.6 节）：每个写操作必须有成功/失败反馈。
import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  type ReactNode,
} from "react";

type ToastKind = "success" | "error" | "info";

interface Toast {
  id: number;
  kind: ToastKind;
  title: string;
  detail?: string;
}

interface ToastContextValue {
  success: (title: string, detail?: string) => void;
  error: (title: string, detail?: string) => void;
  info: (title: string, detail?: string) => void;
}

const ToastContext = createContext<ToastContextValue | null>(null);

let nextId = 1;

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const timers = useRef<Record<number, ReturnType<typeof setTimeout>>>({});

  const push = useCallback((kind: ToastKind, title: string, detail?: string) => {
    const id = nextId++;
    setToasts((prev) => [...prev.slice(-4), { id, kind, title, detail }]);
    timers.current[id] = setTimeout(() => {
      setToasts((prev) => prev.filter((t) => t.id !== id));
      delete timers.current[id];
    }, 5000);
  }, []);

  const dismiss = useCallback((id: number) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
    const timer = timers.current[id];
    if (timer) {
      clearTimeout(timer);
      delete timers.current[id];
    }
  }, []);

  return (
    <ToastContext.Provider
      value={{
        success: (t, d) => push("success", t, d),
        error: (t, d) => push("error", t, d),
        info: (t, d) => push("info", t, d),
      }}
    >
      {children}
      {/* 右上角 toast 栈 */}
      <div className="fixed top-4 right-4 z-[100] flex w-80 flex-col gap-2">
        {toasts.map((t) => (
          <div
            key={t.id}
            role="status"
            className={`pointer-events-auto rounded-lg border p-3 shadow-lg backdrop-blur ${
              t.kind === "success"
                ? "border-green-200 bg-green-50 text-green-800"
                : t.kind === "error"
                  ? "border-red-200 bg-red-50 text-red-800"
                  : "border-slate-200 bg-white text-slate-800"
            }`}
          >
            <div className="flex items-start justify-between gap-2">
              <div className="min-w-0">
                <div className="text-sm font-medium">{t.title}</div>
                {t.detail && (
                  <div className="mt-1 text-xs opacity-80 break-all">{t.detail}</div>
                )}
              </div>
              <button
                aria-label="关闭"
                onClick={() => dismiss(t.id)}
                className="shrink-0 rounded p-0.5 text-xs opacity-50 hover:opacity-100"
              >
                ✕
              </button>
            </div>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}

export function useToast(): ToastContextValue {
  const ctx = useContext(ToastContext);
  if (!ctx) throw new Error("useToast 必须在 ToastProvider 内使用");
  return ctx;
}
