import { Fragment, useId } from "react";
import { type InstalledApp, type AiAppInfo, type AiUninstallRisk } from "../bindings";

/** 受保护条目的兜底原因：后端一般会给出 protectedReason，缺失时也要把话说清楚。 */
const PROTECTED_FALLBACK_REASON =
  "该条目属于系统组件或系统依赖，CleanSweep 不允许卸载。";

/**
 * 长路径按反斜杠插 <wbr>：断行落在目录分隔符后，不会把 setup.exe 切成两半。
 * <wbr> 不产生字符，复制出来仍是原串。
 */
function breakablePath(text: string) {
  const parts = text.split("\\");
  return parts.map((part, i) => (
    <Fragment key={i}>
      {part}
      {i < parts.length - 1 ? (
        <>
          {"\\"}
          <wbr />
        </>
      ) : null}
    </Fragment>
  ));
}

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
  const hot = aiRisk && (aiRisk.riskLevel === "high" || aiRisk.riskLevel === "critical");

  return (
    <div className="insp">
      {/* 左：安装事实。路径独占一段，其余键值右对齐成读数表。 */}
      <div className="insp-kv">
        <div className="kv-block">
          <span className="k">安装目录</span>
          <span className="v">
            {app.installLocation ? breakablePath(app.installLocation) : "注册表未记录安装目录"}
          </span>
        </div>
        <div className="kvr">
          <span className="k">发布者</span>
          <span className="v">{app.publisher ?? "—"}</span>
        </div>
        <div className="kvr">
          <span className="k">版本</span>
          <span className="v">{app.version ?? "—"}</span>
        </div>
        <div className="kvr">
          <span className="k">安装日期</span>
          <span className="v">{app.installDate ?? "—"}</span>
        </div>
        <div className="kvr">
          <span className="k">来源</span>
          <span className="v">{app.source}</span>
        </div>
        {app.uninstallString && (
          <div className="kv-block">
            <span className="k">卸载串</span>
            <span className="v">{breakablePath(app.uninstallString)}</span>
          </div>
        )}
      </div>

      {/* 中：判定。读数在上，AI 的文字说明在下面限高滚动。 */}
      <div className="insp-verdict">
        {isProtected && (
          <div className="protected-note" id={protectedNoteId} role="alert">
            <div className="note-head">
              <span className="tag protected">受保护</span>
              <strong>不允许卸载</strong>
            </div>
            <p className="reason">{protectedReason}</p>
            <p className="hint">
              为避免系统不可用，卸载入口已禁用。你仍可以使用 AI 分析查看它的作用。
            </p>
          </div>
        )}

        <div className="readout">
          <div>
            <span className="k">AI 风险</span>
            <b className={hot ? "hot" : undefined}>
              {aiRisk ? riskLabel(aiRisk.riskLevel) : "—"}
            </b>
          </div>
          <div>
            <span className="k">依赖项</span>
            <b>{aiRisk ? aiRisk.dependencies.length : "—"}</b>
          </div>
          <div>
            <span className="k">受影响组件</span>
            <b>{aiRisk ? aiRisk.affectedComponents.length : "—"}</b>
          </div>
        </div>

        <div className="verdict-body">
          {aiInfo && (
            <div className="verdict-line">
              <span className={"pill " + (aiInfo.isSafeToRemove ? "safe" : "warn")}>
                {aiInfo.isSafeToRemove ? "AI 认为可卸载" : "AI 建议保留"}
              </span>
              {aiInfo.category && <span className="tag">{aiInfo.category}</span>}
            </div>
          )}

          {aiInfo && <p className="body">{aiInfo.summary}</p>}

          {aiInfo && aiInfo.alternatives.length > 0 && (
            <p className="body" style={{ marginTop: 6 }}>
              <span className="muted">替代品：</span>
              {aiInfo.alternatives.join("、")}
            </p>
          )}

          {aiInfo && aiInfo.riskNotes.length > 0 && (
            <ul className="body">
              {aiInfo.riskNotes.map((n, i) => (
                <li key={i}>{n}</li>
              ))}
            </ul>
          )}

          {aiRisk && (
            <>
              <div className="verdict-line" style={{ marginTop: aiInfo ? 10 : 0 }}>
                <span className={"pill " + riskPill(aiRisk.riskLevel)}>
                  卸载风险 {riskLabel(aiRisk.riskLevel)}
                </span>
              </div>
              <p className="body">{aiRisk.recommendation}</p>
              {aiRisk.dependencies.length > 0 && (
                <ul className="body">
                  {aiRisk.dependencies.map((n, i) => (
                    <li key={i}>{n}</li>
                  ))}
                </ul>
              )}
              {aiRisk.affectedComponents.length > 0 && (
                <ul className="body warn">
                  {aiRisk.affectedComponents.map((n, i) => (
                    <li key={i}>{n}</li>
                  ))}
                </ul>
              )}
            </>
          )}

          {!aiInfo && !aiRisk && (
            <p className="body">
              还没有分析过这一项。AI 会说明它的作用、可替代的软件，以及卸载会影响到什么。
            </p>
          )}
        </div>
      </div>

      {/* 右：动作。与中栏用竖线隔开，滚不滚都停在原地。 */}
      <div className="insp-act">
        <div className="acts">
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
            {aiBusy ? <span className="spinner" /> : null}
            {aiBusy ? "分析中" : aiInfo || aiRisk ? "重新分析" : "AI 分析用途与风险"}
          </button>
          <p className="hint">
            {isProtected
              ? "受保护条目不会出现在卸载向导里，后端也会再次拒绝。"
              : "卸载前会先扫描残留（文件、注册表、服务、计划任务、快捷方式），逐项确认后才删除。"}
          </p>
        </div>
      </div>
    </div>
  );
}

function riskLabel(r: AiUninstallRisk["riskLevel"]) {
  return { low: "低", medium: "中", high: "高", critical: "极高" }[r];
}
function riskPill(r: AiUninstallRisk["riskLevel"]) {
  return r === "low" ? "safe" : r === "medium" ? "warn" : "danger";
}
