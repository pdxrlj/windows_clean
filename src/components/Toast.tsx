import { createContext, useCallback, useContext, useState } from "react";

export interface Toast {
  id: number;
  msg: string;
  kind: "ok" | "err" | "info";
}

export function useToasts() {
  const [list, setList] = useState<Toast[]>([]);
  const push = useCallback((msg: string, kind: Toast["kind"] = "info") => {
    const id = Date.now() + Math.random();
    setList((l) => [...l, { id, msg, kind }]);
    setTimeout(() => setList((l) => l.filter((t) => t.id !== id)), 4500);
  }, []);
  return { list, push };
}

export const ToastCtx = createContext<{ push: (m: string, k?: Toast["kind"]) => void }>({
  push: () => {},
});

export function Toasts({ toasts }: { toasts: Toast[] }) {
  return (
    <div className="toast-wrap">
      {toasts.map((t) => (
        <div key={t.id} className={"toast " + (t.kind === "ok" ? "ok" : t.kind === "err" ? "err" : "")}>
          {t.msg}
        </div>
      ))}
    </div>
  );
}
