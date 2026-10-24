import { StrictMode, useEffect, useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import "../../src/styles.css";
import { SettingsView } from "../../src/views/SettingsView";
import { UninstallWizard } from "../../src/views/UninstallWizard";
import { AppsView } from "../../src/views/AppsView";
import { HistoryView } from "../../src/views/HistoryView";
import { PermissionsView } from "../../src/views/PermissionsView";
import { ToastCtx, Toasts, useToasts } from "../../src/components/Toast";
import { ConfirmProvider } from "../../src/components/Confirm";
import { api, type InstalledApp } from "../../src/bindings";
import { previewWizardApp, SAMPLE_APPS } from "./tauri-core-mock";

const TABS = [
  { id: "apps", label: "软件" },
  { id: "history", label: "卸载历史" },
  { id: "permissions", label: "应用权限" },
  { id: "settings", label: "设置" },
];

/** 与 src/App.tsx 的 formatKb 保持一致，预览里只用来填读数条。 */
function formatKb(kb: number) {
  if (!kb) return "—";
  if (kb >= 1024 * 1024) return `${(kb / 1024 / 1024).toFixed(2)} GB`;
  if (kb >= 1024) return `${(kb / 1024).toFixed(1)} MB`;
  return `${kb} KB`;
}

/** Browser-only preview of CleanSweep views (Tauri APIs are mocked). */
function Preview() {
  const toasts = useToasts();
  const [ready, setReady] = useState(false);
  const params = new URLSearchParams(location.search);
  const view = params.get("view") ?? "settings";

  useEffect(() => {
    api.resolveAiEffective().then((e) => setReady(e.ready)).catch(() => {});
  }, []);

  const apps = SAMPLE_APPS as unknown as InstalledApp[];
  let cleanable = 0;
  let keep = 0;
  for (const a of apps) {
    if (a.isProtected) keep += a.estimatedSizeKb ?? 0;
    else cleanable += a.estimatedSizeKb ?? 0;
  }
  const total = cleanable + keep;

  // 预览外壳刻意与真实 App 同构：class 名一致，截图才有参考价值。
  const shell = (active: string, children: ReactNode) => (
    <ToastCtx.Provider value={toasts}>
      <ConfirmProvider>
        <div className="app">
          <header className="top">
            <div className="brand">
              <span className="mark" aria-hidden>
                <i />
              </span>
              <span className="name">CLEANSWEEP</span>
            </div>
            <div className="reads">
              <div className="read">
                <span className="lbl">已安装</span>
                <b>{apps.length}</b>
              </div>
              <div className="read">
                <span className="lbl">受保护</span>
                <b>{apps.filter((a) => a.isProtected).length}</b>
              </div>
              <div className="read">
                <span className="lbl">已知占用</span>
                <b className="hot">{formatKb(total)}</b>
              </div>
            </div>
            <div className="lamps">
              <span className="lamp on" title="已以管理员身份运行">
                <i />
                管理员
              </span>
              <span className={"lamp" + (ready ? " on" : "")} title={ready ? "AI 已启用" : "AI 未配置"}>
                <i />
                AI
              </span>
            </div>
          </header>
          <div className="body">
            <nav className="rail">
              {TABS.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  className={"seg" + (t.id === active ? " active" : "")}
                >
                  <span className="dot" aria-hidden />
                  {t.label}
                  <span className="n">
                    {t.id === "apps" || t.id === "permissions" ? apps.length : ""}
                  </span>
                </button>
              ))}
              <div className="foot">
                <span className="lbl">容量构成</span>
                <div className="meter" aria-hidden>
                  <i className="ok" style={{ width: `${total ? (cleanable / total) * 100 : 0}%` }} />
                  <i
                    className="keep"
                    style={{ width: `${total ? (keep / total) * 100 : 100}%` }}
                  />
                </div>
                <div className="note">
                  可清理 {formatKb(cleanable)}
                  <br />
                  受保护 {formatKb(keep)}
                </div>
              </div>
            </nav>
            <main className="main">{children}</main>
          </div>
        </div>
        <Toasts toasts={toasts.list} />
      </ConfirmProvider>
    </ToastCtx.Provider>
  );

  if (view === "wizard") {
    const app = previewWizardApp() as unknown as InstalledApp;
    return shell(
      "apps",
      <>
        <div className="topbar">
          <span className="h">软件</span>
        </div>
        <div className="content">
          <div className="settings-page" style={{ opacity: 0.25 }}>
            <div className="settings-card">（卸载向导背景）</div>
          </div>
        </div>
        <UninstallWizard app={app} onClose={() => {}} onDone={() => {}} />
      </>
    );
  }

  if (view === "apps") {
    return shell("apps", <AppsView apps={apps} loading={false} onRefresh={() => {}} />);
  }

  if (view === "history") {
    return shell("history", <HistoryView />);
  }

  if (view === "permissions") {
    return shell("permissions", <PermissionsView apps={apps} />);
  }

  return shell(
    "settings",
    <SettingsView onSaved={() => api.resolveAiEffective().then((e) => setReady(e.ready))} />
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Preview />
  </StrictMode>
);
