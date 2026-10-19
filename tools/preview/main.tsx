import { StrictMode, useEffect, useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import "../../src/styles.css";
import { SettingsView } from "../../src/views/SettingsView";
import { UninstallWizard } from "../../src/views/UninstallWizard";
import { AppsView } from "../../src/views/AppsView";
import { PermissionsView } from "../../src/views/PermissionsView";
import { ToastCtx, Toasts, useToasts } from "../../src/components/Toast";
import { ConfirmProvider } from "../../src/components/Confirm";
import { api, type InstalledApp } from "../../src/bindings";
import { previewWizardApp, SAMPLE_APPS } from "./tauri-core-mock";

/** Browser-only preview of CleanSweep views (Tauri APIs are mocked). */
function Preview() {
  const toasts = useToasts();
  const [ready, setReady] = useState(false);
  const params = new URLSearchParams(location.search);
  const view = params.get("view");

  useEffect(() => {
    api.resolveAiEffective().then((e) => setReady(e.ready)).catch(() => {});
  }, []);

  const shell = (children: ReactNode) => (
    <ToastCtx.Provider value={toasts}>
      <ConfirmProvider>
        <div className="app">
          <nav className="rail">
            <div className="glyph" title="CleanSweep">CS</div>
            <div className="nav" title="软件管理"><span aria-hidden>⊞</span></div>
            <div className="nav" title="应用权限"><span aria-hidden>⊘</span></div>
            <div className="nav active" title="设置"><span aria-hidden>≡</span></div>
            <div className="spacer" />
            <div className={"ai-dot" + (ready ? " on" : "")} title={ready ? "AI 已启用" : "AI 未配置"} />
          </nav>
          <main className="main">{children}</main>
        </div>
        <Toasts toasts={toasts.list} />
      </ConfirmProvider>
    </ToastCtx.Provider>
  );

  if (view === "wizard") {
    const app = previewWizardApp() as unknown as InstalledApp;
    return shell(
      <>
        <div className="topbar">
          <span className="title">软件管理</span>
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
    return shell(<AppsView />);
  }

  if (view === "permissions") {
    return shell(
      <PermissionsView apps={SAMPLE_APPS as unknown as InstalledApp[]} />
    );
  }

  return shell(
    <SettingsView onSaved={() => api.resolveAiEffective().then((e) => setReady(e.ready))} />
  );
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <Preview />
  </StrictMode>
);
