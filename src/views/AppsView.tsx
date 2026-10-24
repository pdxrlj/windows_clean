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

/** 行高固定 44px，尺寸列只放得下一次读数，单位保持短。 */
function formatSize(kb?: number | null) {
  if (!kb) return "—";
  if (kb >= 1024 * 1024) return `${(kb / 1024 / 1024).toFixed(2)} GB`;
  return `${(kb / 1024).toFixed(1)} MB`;
}

export function AppsView({
  apps,
  loading,
  onRefresh,
}: {
  apps: InstalledApp[];
  loading: boolean;
  onRefresh: () => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<Filter>("all");
  // 展开行的 id 由这一层持有：列表是单列平铺，同一时刻只展开一个仪表。
  const [openId, setOpenId] = useState<string | null>(null);
  const [wizard, setWizard] = useState<InstalledApp | null>(null);
  const [aiInfo, setAiInfo] = useState<AiAppInfo | null>(null);
  const [aiRisk, setAiRisk] = useState<AiUninstallRisk | null>(null);
  const [aiBusy, setAiBusy] = useState(false);
  const confirm = useConfirm();
  const searchRef = useRef<HTMLInputElement>(null);
  // 防止 AI 分析请求返回时用户已经展开别的软件，导致 A 的结论显示在 B 上。
  const openIdRef = useRef<string | null>(null);

  useEffect(() => {
    const unsubs: Array<() => void> = [];
    onEvent<string>("uninstall-log", () => {}).then((u) => unsubs.push(() => u()));
    return () => unsubs.forEach((f) => f());
  }, []);

  // Ctrl/⌘ + K 聚焦搜索：提示标在输入框右侧，就必须真的能按。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
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

  const selected = useMemo(
    () => apps.find((a) => a.id === openId) ?? null,
    [apps, openId]
  );

  const onSelect = (app: InstalledApp) => {
    if (openIdRef.current === app.id) {
      openIdRef.current = null;
      setOpenId(null);
      return;
    }
    openIdRef.current = app.id;
    setOpenId(app.id);
    setAiInfo(null);
    setAiRisk(null);
  };

  // 受保护条目绝不进入卸载向导：列表/详情是第一道 UI 闸门，向导与后端还有各自防线。
  const openUninstall = (app: InstalledApp) => {
    if (app.isProtected) return;
    setWizard(app);
  };

  const analyze = async (app: InstalledApp) => {
    const target = app;
    setAiBusy(true);
    try {
      const [info, risk] = await Promise.all([
        api.aiAnalyzeApp(target).catch((e) => ({ err: String(e) } as any)),
        api.aiAnalyzeRisk(target).catch((e) => ({ err: String(e) } as any)),
      ]);
      if (openIdRef.current !== target.id) return; // 用户已切换软件，丢弃旧结果
      if (info?.err) throw new Error(info.err);
      if (risk?.err) throw new Error(risk.err);
      setAiInfo(info);
      setAiRisk(risk);
    } catch (e: any) {
      if (openIdRef.current !== target.id) return;
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
    <div className="pane">
      <div className="topbar">
        <div className="searchbox">
          <span className="lbl" aria-hidden>
            查找
          </span>
          <input
            ref={searchRef}
            placeholder="软件名或发布者"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-label="搜索软件名或发布者"
          />
          <span className="k" aria-hidden>
            Ctrl K
          </span>
        </div>
        <select
          value={filter}
          onChange={(e) => setFilter(e.target.value as Filter)}
          style={{ width: 148 }}
          aria-label="按来源筛选"
        >
          <option value="all">全部来源</option>
          <option value="user">当前用户</option>
          <option value="machine">本机(所有用户)</option>
          <option value="store">Store 应用</option>
          <option value="system">系统组件</option>
        </select>
        <div className="spacer" />
        <span className="count">
          {filtered.length} / {apps.length} 项
        </span>
        <button onClick={onRefresh} disabled={loading}>
          {loading ? <span className="spinner" /> : "重新枚举"}
        </button>
      </div>

      <div className="rows">
        {/* 表头放进滚动容器内部：sticky 之后与行同宽，出现滚动条也不会错位。 */}
        <div className="thead" aria-hidden>
          <span>软件</span>
          <span>发布者 / 版本</span>
          <span>来源</span>
          <span className="num">大小</span>
        </div>
        {filtered.map((a) => {
          const open = openId === a.id;
          return (
            <div className="item" key={a.id}>
              <button
                type="button"
                className={"app-row" + (open ? " open" : "")}
                onClick={() => onSelect(a)}
                aria-expanded={open}
              >
                <span className="nm">
                  <span className="nm-t">{a.displayName}</span>
                  {a.isSystemComponent && <span className="tag">sys</span>}
                  {a.isProtected && (
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
                  )}
                </span>
                <span className="meta">
                  {a.publisher ?? "未知发布者"} · {a.version ?? "—"}
                </span>
                <span className="src">{sourceLabel(a.source)}</span>
                <span className="size">{formatSize(a.estimatedSizeKb)}</span>
              </button>
              {open && selected && (
                <AppDetail
                  app={selected}
                  aiInfo={aiInfo}
                  aiRisk={aiRisk}
                  aiBusy={aiBusy}
                  onAnalyze={() => analyze(selected)}
                  onUninstall={() => openUninstall(selected)}
                />
              )}
            </div>
          );
        })}
        {!loading && filtered.length > 0 && (
          <div className="rows-end">已列出全部 {filtered.length} 项</div>
        )}
        {!loading && filtered.length === 0 && (
          <div className="rows-empty">
            {apps.length === 0
              ? "没有枚举到已安装软件。"
              : "没有匹配的软件，试试清空搜索或换一个来源。"}
          </div>
        )}
        {loading && apps.length === 0 && (
          <div className="rows-empty">正在枚举注册表与 Store 应用…</div>
        )}
      </div>

      {wizard && (
        <UninstallWizard
          app={wizard}
          onClose={() => setWizard(null)}
          onDone={() => {
            setWizard(null);
            onRefresh();
            openIdRef.current = null;
            setOpenId(null);
          }}
        />
      )}
    </div>
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
