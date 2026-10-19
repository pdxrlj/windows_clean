import { useId } from "react";
import { type InstalledApp, type AiAppInfo, type AiUninstallRisk } from "../bindings";

/** 受保护条目的兜底原因：后端一般会给出 protectedReason，缺失时也要把话说清楚。 */
const PROTECTED_FALLBACK_REASON =
  "该条目属于系统组件或系统依赖，CleanSweep 不允许卸载。";

export function AppDetail({
  app,
  aiInfo,
  aiRisk,
  aiBusy,
  onAnalyze,
  onUninstall,
}: {
  app: InstalledApp;
  aiInfo: AiAppInfo | null;
  aiRisk: AiUninstallRisk | null;
  aiBusy: boolean;
  onAnalyze: () => void;
  onUninstall: () => void;
}) {
  const protectedNoteId = useId();
  // 旧版后端可能不返回 isProtected，缺失一律按“不受保护”处理。
  const isProtected = Boolean(app.isProtected);
  const protectedReason = isProtected
    ? app.protectedReason?.trim() || PROTECTED_FALLBACK_REASON
    : null;
  const uninstallTitle = isProtected
    ? `该条目受系统保护，CleanSweep 不允许卸载：${protectedReason}`
    : undefined;

  return (
    <>
      <div className="section">
        <div className="h">软件</div>
        <div className="row" style={{ marginBottom: 4 }}>
          <span className="title" style={{ flex: 1 }}>{app.displayName}</span>
          {aiInfo && (
            <span className={"pill " + (aiInfo.isSafeToRemove ? "safe" : "warn")}>
              {aiInfo.isSafeToRemove ? "AI 可卸载" : "AI 谨慎"}
            </span>
          )}
        </div>
        {isProtected && (
          <div className="protected-note" id={protectedNoteId} role="alert">
            <div className="note-head">
              <span className="tag protected">受保护</span>
              <strong>该条目受系统保护，CleanSweep 不允许卸载</strong>
            </div>
            <p className="reason">{protectedReason}</p>
            <p className="hint">
              为避免系统不可用，卸载入口已禁用。你仍可以查看详情或使用 AI 分析。
            </p>
          </div>
        )}
        <div className="kv">
          <span className="k">发布者</span><span className="v">{app.publisher ?? "—"}</span>
          <span className="k">版本</span><span className="v">{app.version ?? "—"}</span>
          <span className="k">安装目录</span><span className="v mono">{app.installLocation ?? "—"}</span>
          <span className="k">安装日期</span><span className="v">{app.installDate ?? "—"}</span>
          <span className="k">大小</span><span className="v">{app.estimatedSizeKb ? `${(app.estimatedSizeKb / 1024).toFixed(1)} MB` : "—"}</span>
          <span className="k">来源</span><span className="v">{app.source}</span>
          {app.uninstallString && (
            <>
              <span className="k">卸载串</span>
              <span className="v mono">{app.uninstallString}</span>
            </>
          )}
        </div>
        <div className="actions" style={{ marginTop: 14 }}>
          <button
            className="primary"
            onClick={() => {
              if (!isProtected) onUninstall();
            }}
            disabled={isProtected}
            aria-disabled={isProtected}
            aria-describedby={isProtected ? protectedNoteId : undefined}
            title={uninstallTitle}
          >
            卸载
          </button>
          <button onClick={onAnalyze} disabled={aiBusy}>
            {aiBusy ? <span className="spinner" /> : null} AI 分析用途与风险
          </button>
        </div>
      </div>

      {aiInfo && (
        <div className="section">
          <div className="h">AI 用途分析</div>
          <p style={{ margin: "0 0 10px" }}>{aiInfo.summary}</p>
          <div className="kv">
            {aiInfo.category && (<><span className="k">类别</span><span className="v">{aiInfo.category}</span></>)}
            {aiInfo.alternatives.length > 0 && (
              <><span className="k">替代品</span><span className="v">{aiInfo.alternatives.join("、")}</span></>
            )}
          </div>
          {aiInfo.riskNotes.length > 0 && (
            <ul style={{ margin: "8px 0 0", paddingLeft: 18, color: "var(--dim)", fontSize: 12 }}>
              {aiInfo.riskNotes.map((n, i) => <li key={i}>{n}</li>)}
            </ul>
          )}
        </div>
      )}

      {aiRisk && (
        <div className="section">
          <div className="h">AI 卸载风险</div>
          <div className="row" style={{ marginBottom: 8 }}>
            <span className={"pill " + riskPill(aiRisk.riskLevel)}>
              风险 {riskLabel(aiRisk.riskLevel)}
            </span>
          </div>
          <div className="kv">
            <span className="k">建议</span><span className="v">{aiRisk.recommendation}</span>
          </div>
          {aiRisk.dependencies.length > 0 && (
            <ul style={{ margin: "8px 0 0", paddingLeft: 18, color: "var(--dim)", fontSize: 12 }}>
              {aiRisk.dependencies.map((n, i) => <li key={i}>{n}</li>)}
            </ul>
          )}
          {aiRisk.affectedComponents.length > 0 && (
            <ul style={{ margin: "8px 0 0", paddingLeft: 18, color: "var(--warn)", fontSize: 12 }}>
              {aiRisk.affectedComponents.map((n, i) => <li key={i}>{n}</li>)}
            </ul>
          )}
        </div>
      )}
    </>
  );
}

function riskLabel(r: AiUninstallRisk["riskLevel"]) {
  return { low: "低", medium: "中", high: "高", critical: "极高" }[r];
}
function riskPill(r: AiUninstallRisk["riskLevel"]) {
  return r === "low" ? "safe" : r === "medium" ? "warn" : "danger";
}
