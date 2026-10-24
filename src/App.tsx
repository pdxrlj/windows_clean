import { useCallback, useEffect, useMemo, useState } from "react";
import { api, type InstalledApp } from "./bindings";
import { AppsView } from "./views/AppsView";
import { HistoryView } from "./views/HistoryView";
import { PermissionsView } from "./views/PermissionsView";
import { SettingsView } from "./views/SettingsView";
import { Toasts, ToastCtx, useToasts } from "./components/Toast";
import { ConfirmProvider } from "./components/Confirm";

type Tab = "apps" | "history" | "permissions" | "settings";

const TABS: Array<{ id: Tab; label: string }> = [
  { id: "apps", label: "软件" },
  { id: "history", label: "卸载历史" },
  { id: "permissions", label: "应用权限" },
  { id: "settings", label: "设置" },
];

/** KB → 人类可读体积。枚举未完成或后端没给估计值时返回占位符。 */
function formatKb(kb: number) {
  if (!kb) return "—";
  if (kb >= 1024 * 1024) return `${(kb / 1024 / 1024).toFixed(2)} GB`;
  if (kb >= 1024) return `${(kb / 1024).toFixed(1)} MB`;
  return `${kb} KB`;
}

export default function App() {
  const [tab, setTab] = useState<Tab>("apps");
  const [aiEnabled, setAiEnabled] = useState(false);
  /** null=检测中；false=非管理员（dev/debug 常见），显示提权提示。 */
  const [elevated, setElevated] = useState<boolean | null>(null);
  const toasts = useToasts();

  // 软件列表提升到这一层：顶部读数与“应用权限”页都要用同一份枚举结果，
  // 一次枚举（约 1–5 秒）够整个应用用，卸载后再统一刷新。
  const [apps, setApps] = useState<InstalledApp[]>([]);
  const [appsLoading, setAppsLoading] = useState(true);

  const refreshApps = useCallback(() => {
    setAppsLoading(true);
    api
      .listInstalledApps()
      .then((list) => {
        list.sort((a, b) => a.displayName.localeCompare(b.displayName, "zh"));
        setApps(list);
      })
      .catch(() => {})
      .finally(() => setAppsLoading(false));
  }, []);

  useEffect(() => {
    refreshApps();
  }, [refreshApps]);

  useEffect(() => {
    // 旧后端没有该命令时按“已提权”处理，避免误报。
    api.isElevated().then(setElevated).catch(() => setElevated(true));
  }, []);

  const refreshAi = () => {
    api
      .resolveAiEffective()
      .then((e) => setAiEnabled(e.ready))
      .catch(() => setAiEnabled(false));
  };

  useEffect(() => {
    refreshAi();
  }, []);

  useEffect(() => {
    const go = () => setTab("settings");
    window.addEventListener("go-settings", go);
    return () => window.removeEventListener("go-settings", go);
  }, []);

  const readout = useMemo(() => {
    let cleanable = 0;
    let keep = 0;
    let protectedCount = 0;
    for (const a of apps) {
      const kb = a.estimatedSizeKb ?? 0;
      if (a.isProtected) {
        keep += kb;
        protectedCount += 1;
      } else {
        cleanable += kb;
      }
    }
    const total = cleanable + keep;
    return {
      cleanable,
      keep,
      protectedCount,
      total,
      cleanableRatio: total > 0 ? (cleanable / total) * 100 : 0,
    };
  }, [apps]);

  const tabCount = (id: Tab) => {
    // 只有与软件列表同源的页签才挂数字，避免凭空编造计数。
    if (id === "apps") return apps.length || "";
    if (id === "permissions") return apps.length || "";
    return "";
  };

  return (
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
                <b>{appsLoading && apps.length === 0 ? "…" : apps.length}</b>
              </div>
              <div className="read">
                <span className="lbl">受保护</span>
                <b>{readout.protectedCount}</b>
              </div>
              <div className="read">
                <span className="lbl">已知占用</span>
                <b className="hot">{formatKb(readout.total)}</b>
              </div>
            </div>
            <div className="lamps">
              <span
                className={
                  "lamp" + (elevated === false ? " warn" : elevated ? " on" : "")
                }
                title={
                  elevated === null
                    ? "正在检测权限"
                    : elevated
                      ? "已以管理员身份运行"
                      : "未以管理员身份运行，部分操作可能失败"
                }
              >
                <i />
                管理员
              </span>
              <span
                className={"lamp" + (aiEnabled ? " on" : "")}
                title={aiEnabled ? "AI 已配置可用" : "AI 未配置，详情页分析不可用"}
              >
                <i />
                AI
              </span>
            </div>
          </header>

          <div className="body">
            <nav className="rail" aria-label="主导航">
              {TABS.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  className={"seg" + (tab === t.id ? " active" : "")}
                  onClick={() => setTab(t.id)}
                  aria-current={tab === t.id ? "page" : undefined}
                >
                  <span className="dot" aria-hidden />
                  {t.label}
                  <span className="n">{tabCount(t.id)}</span>
                </button>
              ))}
              <div className="foot">
                <span className="lbl">容量构成</span>
                <div className="meter" aria-hidden>
                  <i className="ok" style={{ width: `${readout.cleanableRatio}%` }} />
                  <i className="keep" style={{ width: `${100 - readout.cleanableRatio}%` }} />
                </div>
                <div className="note">
                  可清理 {formatKb(readout.cleanable)}
                  <br />
                  受保护 {formatKb(readout.keep)}
                </div>
              </div>
            </nav>

            <main className="main">
              {elevated === false && (
                <div className="admin-banner" role="alert">
                  <span>
                    当前<strong>未以管理员身份运行</strong>（可能取消了 UAC 授权）：卸载、清理残留、防火墙等操作可能失败（如 os error 740）。
                  </span>
                  <span className="admin-banner-how">
                    重启 CleanSweep 并在 UAC 弹框中选择「是」；开发调试时也可在管理员终端执行
                    <code>pnpm tauri dev</code>。
                  </span>
                  <button className="ghost" onClick={() => setElevated(true)}>
                    知道了
                  </button>
                </div>
              )}
              {tab === "apps" && (
                <AppsView
                  apps={apps}
                  loading={appsLoading}
                  onRefresh={refreshApps}
                />
              )}
              {tab === "history" && <HistoryView />}
              {tab === "permissions" && <PermissionsView apps={apps} />}
              {tab === "settings" && <SettingsView onSaved={refreshAi} />}
            </main>
          </div>
        </div>
        <Toasts toasts={toasts.list} />
      </ConfirmProvider>
    </ToastCtx.Provider>
  );
}
