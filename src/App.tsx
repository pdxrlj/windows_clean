import { useEffect, useState } from "react";
import { api, type InstalledApp } from "./bindings";
import { AppsView } from "./views/AppsView";
import { PermissionsView } from "./views/PermissionsView";
import { SettingsView } from "./views/SettingsView";
import { Toasts, ToastCtx, useToasts } from "./components/Toast";
import { ConfirmProvider } from "./components/Confirm";

type Tab = "apps" | "permissions" | "settings";

export default function App() {
  const [tab, setTab] = useState<Tab>("apps");
  const [aiEnabled, setAiEnabled] = useState(false);
  const [apps, setApps] = useState<InstalledApp[]>([]);
  const toasts = useToasts();

  const refreshAi = () => {
    api
      .resolveAiEffective()
      .then((e) => setAiEnabled(e.ready))
      .catch(() => setAiEnabled(false));
  };

  useEffect(() => {
    refreshAi();
  }, []);

  // 只在进入“应用权限”页时枚举软件：避免与 AppsView 的启动枚举重复（每次枚举约 1–5 秒），
  // 同时保证卸载后重新进入权限页能拿到最新列表。
  useEffect(() => {
    if (tab !== "permissions") return;
    api.listInstalledApps().then(setApps).catch(() => {});
  }, [tab]);

  useEffect(() => {
    const go = () => setTab("settings");
    window.addEventListener("go-settings", go);
    return () => window.removeEventListener("go-settings", go);
  }, []);

  return (
    <ToastCtx.Provider value={toasts}>
      <ConfirmProvider>
        <div className="app">
          <nav className="rail">
            <div className="glyph" title="CleanSweep">CS</div>
            <div
              className={"nav" + (tab === "apps" ? " active" : "")}
              title="软件管理"
              onClick={() => setTab("apps")}
            >
              <span aria-hidden>⊞</span>
            </div>
            <div
              className={"nav" + (tab === "permissions" ? " active" : "")}
              title="应用权限"
              onClick={() => setTab("permissions")}
            >
              <span aria-hidden>⊘</span>
            </div>
            <div
              className={"nav" + (tab === "settings" ? " active" : "")}
              title="设置"
              onClick={() => setTab("settings")}
            >
              <span aria-hidden>≡</span>
            </div>
            <div className="spacer" />
            <div className={"ai-dot" + (aiEnabled ? " on" : "")} title={aiEnabled ? "AI 已启用" : "AI 未配置"} />
          </nav>
          <main className="main">
            {tab === "apps" && <AppsView />}
            {tab === "permissions" && <PermissionsView apps={apps} />}
            {tab === "settings" && <SettingsView onSaved={refreshAi} />}
          </main>
        </div>
        <Toasts toasts={toasts.list} />
      </ConfirmProvider>
    </ToastCtx.Provider>
  );
}
