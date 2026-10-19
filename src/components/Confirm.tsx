import { createContext, useCallback, useContext, useRef, useState, type ReactNode } from "react";

interface ConfirmOptions {
  title: string;
  message: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
}

type ConfirmFn = (opts: ConfirmOptions) => Promise<boolean>;

export const ConfirmCtx = createContext<ConfirmFn>(async () => false);

export function useConfirm() {
  return useContext(ConfirmCtx);
}

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<ConfirmOptions | null>(null);
  const [resolver, setResolver] = useState<((v: boolean) => void) | null>(null);
  // 同时只允许一个确认框：双击/并发调用时直接返回 false，避免前一个 Promise 永不结束。
  const openRef = useRef(false);

  const confirm = useCallback<ConfirmFn>((opts) => {
    return new Promise<boolean>((resolve) => {
      if (openRef.current) {
        resolve(false);
        return;
      }
      openRef.current = true;
      setState(opts);
      setResolver(() => resolve);
    });
  }, []);

  const close = (v: boolean) => {
    openRef.current = false;
    resolver?.(v);
    setState(null);
    setResolver(null);
  };

  return (
    <ConfirmCtx.Provider value={confirm}>
      {children}
      {state && (
        <div className="backdrop" onClick={() => close(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-head">
              <span className={"h-title" + (state.danger ? " danger" : "")}>{state.title}</span>
            </div>
            <div className="modal-body">{state.message}</div>
            <div className="modal-foot">
              <button onClick={() => close(false)}>{state.cancelLabel ?? "取消"}</button>
              <button
                className={state.danger ? "danger" : "primary"}
                onClick={() => close(true)}
              >
                {state.confirmLabel ?? "确认"}
              </button>
            </div>
          </div>
        </div>
      )}
    </ConfirmCtx.Provider>
  );
}
