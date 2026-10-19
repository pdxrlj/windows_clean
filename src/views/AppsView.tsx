import { useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  type InstalledApp,
  type AiAppInfo,
  type AiUninstallRisk,
  onEvent,
} from "../bindings";
import { useConfirm } from "../components/Confirm";
import { AppDetail } from "./AppDetail";
import { UninstallWizard } from "./UninstallWizard";

type Filter = "all" | "user" | "machine" | "store" | "system";

export function AppsView() {
  const [apps, setApps] = useState<InstalledApp[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  const [selected, setSelected] = useState<InstalledApp | null>(null);
  const [wizard, setWizard] = useState<InstalledApp | null>(null);
  const [aiInfo, setAiInfo] = useState<AiAppInfo | null>(null);
  const [aiRisk, setAiRisk] = useState<AiUninstallRisk | null>(null);
  const [aiBusy, setAiBusy] = useState(false);
  const confirm = useConfirm();
  // 防止 AI 分析请求返回时用户已经切换到别的软件，导致 A 的结论显示在 B 上。
  const selectedIdRef = useRef<string | null>(null);

  const load = () => {
    setLoading(true);
    api
      .listInstalledApps()
      .then((list) => {
        list.sort((a, b) => a.displayName.localeCompare(b.displayName, "zh"));
        setApps(list);
      })
      .catch((e) => console.error(e))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    load();
    const unsubs: Array<() => void> = [];
    onEvent<string>("uninstall-log", () => {}).then((u) => unsubs.push(() => u()));
    return () => unsubs.forEach((f) => f());
  }, []);

  const filtered = useMemo(() => {
    let l = apps;
    if (filter !== "all") {
      l = l.filter((a) => {
        if (filter === "user") return a.source === "registryuser";
        if (filter === "machine")
          return a.source === "registrymachine64" || a.source === "registrymachine32";
        if (filter === "store") return a.source === "store";
        if (filter === "system") return a.isSystemComponent;
        return true;
      });
    }
    if (query.trim()) {
      const q = query.toLowerCase();
      l = l.filter(
        (a) =>
          a.displayName.toLowerCase().includes(q) ||
          (a.publisher ?? "").toLowerCase().includes(q)
      );
    }
    return l;
  }, [apps, query, filter]);

  const onSelect = (app: InstalledApp) => {
    selectedIdRef.current = app.id;
    setSelected(app);
    setAiInfo(null);
    setAiRisk(null);
  };

  // 受保护条目绝不进入卸载向导：列表/详情是第一道 UI 闸门，向导与后端还有各自防线。
  const openUninstall = (app: InstalledApp) => {
    if (app.isProtected) return;
    setWizard(app);
  };

  const analyze = async () => {
    if (!selected) return;
    const target = selected;
    setAiBusy(true);
    try {
      const [info, risk] = await Promise.all([
        api.aiAnalyzeApp(target).catch((e) => ({ err: String(e) } as any)),
        api.aiAnalyzeRisk(target).catch((e) => ({ err: String(e) } as any)),
      ]);
      if (selectedIdRef.current !== target.id) return; // 用户已切换软件，丢弃旧结果
      if (info?.err) throw new Error(info.err);
      if (risk?.err) throw new Error(risk.err);
      setAiInfo(info);
      setAiRisk(risk);
    } catch (e: any) {
      if (selectedIdRef.current !== target.id) return;
      const msg = String(e?.message ?? e);
      const ok = await confirm({
        title: "AI 分析失败",
        message: <div>{msg}</div>,
        confirmLabel: "去设置",
        cancelLabel: "关闭",
      });
      if (ok) window.dispatchEvent(new CustomEvent("go-settings"));
    } finally {
      setAiBusy(false);
    }
  };

  return (
    <>
      <div className="topbar">
        <input
          style={{ width: 280 }}
          placeholder="搜索软件名或发布者"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <select value={filter} onChange={(e) => setFilter(e.target.value as Filter)} style={{ width: 150 }}>
          <option value="all">全部来源</option>
          <option value="user">当前用户</option>
          <option value="machine">本机(所有用户)</option>
          <option value="store">Store 应用</option>
          <option value="system">系统组件</option>
        </select>
        <div className="spacer" />
        <span className="count">{filtered.length} 项</span>
        <button onClick={load} disabled={loading}>
          {loading ? <span className="spinner" /> : "刷新"}
        </button>
      </div>
      <div className="split content">
        <div className="list-col">
          <div className="list-head">
            <span />
            <span>软件</span>
            <span>发布者 / 版本</span>
            <span>来源</span>
            <span style={{ textAlign: "right" }}>大小</span>
          </div>
          {filtered.map((a) => (
            <div
              key={a.id}
              className={"list-row" + (selected?.id === a.id ? " selected" : "")}
              onClick={() => onSelect(a)}
            >
              <span>
                {a.isSystemComponent ? <span className="tag">sys</span> : null}
              </span>
              <span className="name">
                {a.isProtected ? (
                  <span className="name-line">
                    <span className="name-text">{a.displayName}</span>
                    <span
                      className="tag protected"
                      title={
                        a.protectedReason
                          ? `受保护：${a.protectedReason}`
                          : "该条目属于系统组件或系统依赖，CleanSweep 不允许卸载"
                      }
                    >
                      受保护
                    </span>
                  </span>
                ) : (
                  a.displayName
                )}
              </span>
              <span className="meta">
                {a.publisher ?? "未知"} · {a.version ?? "—"}
              </span>
              <span className="src">{sourceLabel(a.source)}</span>
              <span className="size">
                {a.estimatedSizeKb ? `${(a.estimatedSizeKb / 1024).toFixed(1)} MB` : "—"}
              </span>
            </div>
          ))}
          {!loading && filtered.length === 0 && (
            <div className="muted" style={{ padding: 24 }}>没有匹配的软件。</div>
          )}
        </div>
        <div className="detail-col">
          {selected ? (
            <AppDetail
              app={selected}
              aiInfo={aiInfo}
              aiRisk={aiRisk}
              aiBusy={aiBusy}
              onAnalyze={analyze}
              onUninstall={() => openUninstall(selected)}
            />
          ) : (
            <div className="section">
              <div className="h">未选中</div>
              <div className="title">从左侧选择一项</div>
              <div className="sub">
                CleanSweep 枚举注册表与 Store 应用。选中后可让 AI 说明软件作用与卸载风险，或启动卸载向导进行残留扫描与逐项确认删除。
              </div>
            </div>
          )}
        </div>
      </div>
      {wizard && (
        <UninstallWizard
          app={wizard}
          onClose={() => setWizard(null)}
          onDone={() => {
            setWizard(null);
            load();
            selectedIdRef.current = null;
            setSelected(null);
          }}
        />
      )}
    </>
  );
}

function sourceLabel(s: InstalledApp["source"]) {
  switch (s) {
    case "registrymachine64": return "本机64";
    case "registrymachine32": return "本机32";
    case "registryuser": return "用户";
    case "store": return "Store";
    case "steam": return "Steam";
  }
}
