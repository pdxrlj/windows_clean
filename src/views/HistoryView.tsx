import { useEffect, useMemo, useState } from "react";
import {
  api,
  type AiResidualAssessment,
  type AiSafetyVerdict,
  type ResidualItem,
  type UninstallRecord,
} from "../bindings";
import { useConfirm } from "../components/Confirm";

function fmtTime(ms?: number | null): string {
  if (!ms) return "—";
  try {
    return new Date(ms).toLocaleString("zh-CN", { hour12: false });
  } catch {
    return "—";
  }
}

function fmtSize(bytes?: number | null): string {
  if (!bytes || bytes <= 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let n = bytes;
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

const KIND_LABEL: Record<string, string> = {
  file: "文件",
  directory: "目录",
  registrykey: "注册表",
  registryvalue: "启动项",
  shortcut: "快捷方式",
  service: "服务",
  scheduledtask: "计划任务",
  envvar: "环境变量",
};

function verdictLabel(v?: AiSafetyVerdict): string {
  return v === "safe" ? "可删" : v === "caution" ? "谨慎" : v === "keep" ? "保留" : v === "unknown" ? "未知" : "—";
}
function verdictPill(v?: AiSafetyVerdict): string {
  return v === "safe" ? "safe" : v === "caution" ? "warn" : v === "keep" ? "danger" : "";
}

export function HistoryView() {
  const [records, setRecords] = useState<UninstallRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [residuals, setResiduals] = useState<ResidualItem[]>([]);
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [assessment, setAssessment] = useState<AiResidualAssessment | null>(null);
  const [aiReady, setAiReady] = useState<boolean | null>(null);
  const [aiBusy, setAiBusy] = useState(false);
  const [scanBusy, setScanBusy] = useState(false);
  const [scanError, setScanError] = useState<string | null>(null);
  // AI 推断补漏的结果：规则 + 推断合并后，这里记下有多少项是 AI 补出来的。
  const [aiGuess, setAiGuess] = useState<{
    guessed: number;
    summary: string;
    error: string | null;
  } | null>(null);
  const [riskOverride, setRiskOverride] = useState(false);
  const confirm = useConfirm();

  const selectedRecord = useMemo(
    () => records.find((r) => r.id === selectedId) ?? null,
    [records, selectedId],
  );

  // success / 退出码是卸载当时写进库里的，复审只更新残留数；
  // 这里把「卸载器退出码」和「当前还剩多少残留」分开讲，避免被读成同件事。
  const scanSummary = !selectedRecord
    ? ""
    : !selectedRecord.lastScanAt
      ? "还没有复查过残留。"
      : selectedRecord.residualCount === 0
        ? `最近一次复查（${fmtTime(selectedRecord.lastScanAt)}）没有残留，无需再清理。`
        : `最近一次复查（${fmtTime(selectedRecord.lastScanAt)}）还有 ${selectedRecord.residualCount} 项残留待清理。`;

  const load = async () => {
    setLoading(true);
    try {
      const list = await api.listUninstallHistory(200);
      setRecords(list);
      setSelectedId((prev) =>
        prev && list.some((r) => r.id === prev) ? prev : (list[0]?.id ?? null),
      );
    } catch (e) {
      setScanError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
    api
      .resolveAiEffective()
      .then((e) => setAiReady(e.ready))
      .catch(() => setAiReady(false));
  }, []);

  // 切换历史记录：恢复上次扫描结果；AI 结论必须重新评估。
  useEffect(() => {
    if (!selectedRecord) {
      setResiduals([]);
      setSelected({});
      setAssessment(null);
      return;
    }
    setResiduals(selectedRecord.residuals);
    setSelected(
      Object.fromEntries(selectedRecord.residuals.map((r) => [r.id, false])),
    );
    setAssessment(null);
    setRiskOverride(false);
    setScanError(null);
    setAiGuess(null);
  }, [selectedId]);

  const scan = async () => {
    if (!selectedRecord) return;
    setScanBusy(true);
    setScanError(null);
    setAssessment(null);
    setRiskOverride(false);
    setAiGuess(null);
    try {
      let items: ResidualItem[];
      if (aiReady === false) {
        // 没配 AI 时只能跑固定规则，结论的覆盖率受限于名字匹配。
        items = await api.scanHistoryResiduals(selectedRecord.id);
      } else {
        const r = await api.aiSuggestHistoryResiduals(selectedRecord.id);
        items = r.items;
        setAiGuess({
          guessed: r.aiGuessed,
          summary: r.aiSummary,
          error: r.aiError ?? null,
        });
      }
      setResiduals(items);
      setSelected(Object.fromEntries(items.map((r) => [r.id, false])));
      setRecords((prev) =>
        prev.map((r) =>
          r.id === selectedRecord.id
            ? { ...r, residuals: items, residualCount: items.length, lastScanAt: Date.now() }
            : r,
        ),
      );
      if (items.length > 0 && aiReady !== false) {
        setAiBusy(true);
        try {
          setAssessment(await api.aiAssessResiduals(selectedRecord.app, items));
        } catch (e) {
          setScanError(`AI 分析失败：${String(e)}`);
        } finally {
          setAiBusy(false);
        }
      }
    } catch (e) {
      setScanError(String(e));
    } finally {
      setScanBusy(false);
    }
  };

  const verdictOf = (id: string) =>
    assessment?.perItem.find((p) => p.residualId === id)?.verdict;
  const reasonOf = (id: string) =>
    assessment?.perItem.find((p) => p.residualId === id)?.reason;

  const selectedItems = residuals.filter((r) => selected[r.id]);
  const keepItems = selectedItems.filter((r) => verdictOf(r.id) === "keep");
  const cautiousItems = selectedItems.filter((r) => {
    const v = verdictOf(r.id);
    return !v || v === "caution" || v === "unknown";
  });
  const aiGatePending = aiReady !== false && (aiBusy || (residuals.length > 0 && !assessment));
  const deleteNeedsOverride =
    aiReady !== false && cautiousItems.length > 0 && !riskOverride;
  const deleteDisabled =
    selectedItems.length === 0 ||
    keepItems.length > 0 ||
    aiGatePending ||
    deleteNeedsOverride ||
    scanBusy;

  const toggleAll = (on: boolean) => {
    setSelected(Object.fromEntries(residuals.map((r) => [r.id, on])));
  };

  const doDelete = async () => {
    if (!selectedRecord || selectedItems.length === 0) return;
    if (aiReady !== false) {
      if (aiBusy || !assessment) {
        await confirm({
          title: "AI 安全审核未完成",
          message: <p>请等待 AI 审核完成后再删除；审核失败可重新扫描。</p>,
          confirmLabel: "知道了",
          cancelLabel: "关闭",
        });
        return;
      }
      if (keepItems.length > 0) {
        await confirm({
          title: "AI 建议保留这些项目",
          danger: true,
          message: (
            <div>
              <p>以下项目被 AI 判定为不建议删除，请取消勾选：</p>
              <ul>
                {keepItems.map((r) => (
                  <li key={r.id}>
                    {r.path}
                    {reasonOf(r.id) ? ` — ${reasonOf(r.id)}` : ""}
                  </li>
                ))}
              </ul>
            </div>
          ),
          confirmLabel: "知道了",
          cancelLabel: "关闭",
        });
        return;
      }
      if (cautiousItems.length > 0 && !riskOverride) {
        await confirm({
          title: "AI 对部分项目给出谨慎/未知结论",
          message: <p>请先勾选下方「我了解风险，仍要删除」。</p>,
          confirmLabel: "知道了",
          cancelLabel: "关闭",
        });
        return;
      }
    }
    const ok = await confirm({
      title: `确认删除 ${selectedItems.length} 项残留`,
      danger: true,
      message: (
        <div>
          <p>删除不可恢复，将删除：</p>
          <pre style={{ maxHeight: 240, overflow: "auto" }}>
            {selectedItems.map((r) => `[${KIND_LABEL[r.kind] ?? r.kind}] ${r.path}`).join("\n")}
          </pre>
        </div>
      ),
      confirmLabel: "全部删除",
      cancelLabel: "取消",
    });
    if (!ok) return;
    try {
      await api.deleteResiduals(selectedItems);
      const items = await api.scanHistoryResiduals(selectedRecord.id);
      setResiduals(items);
      setSelected(Object.fromEntries(items.map((r) => [r.id, false])));
      setAssessment(null);
      setRiskOverride(false);
      setRecords((prev) =>
        prev.map((r) =>
          r.id === selectedRecord.id
            ? { ...r, residuals: items, residualCount: items.length, lastScanAt: Date.now() }
            : r,
        ),
      );
    } catch (e) {
      setScanError(`删除失败：${String(e)}`);
    }
  };

  const removeRecord = async () => {
    if (!selectedRecord) return;
    const ok = await confirm({
      title: "删除这条卸载历史？",
      danger: true,
      message: (
        <p>
          将删除「{selectedRecord.app.displayName}」的本地记录（不会删除任何软件或残留文件）。
        </p>
      ),
      confirmLabel: "删除记录",
      cancelLabel: "取消",
    });
    if (!ok) return;
    try {
      await api.deleteUninstallHistory(selectedRecord.id);
      setSelectedId(null);
      await load();
    } catch (e) {
      setScanError(`删除历史失败：${String(e)}`);
    }
  };

  return (
    <>
      <div className="topbar">
        <span className="h" style={{ marginRight: 8 }}>卸载历史</span>
        <span className="muted" style={{ fontSize: 12 }}>
          本地 SQLite 记录，可随时重新扫描残留并让 AI 复查
        </span>
        <div className="spacer" />
        <span className="count">{records.length} 条</span>
        <button onClick={() => void load()} disabled={loading}>
          {loading ? <span className="spinner" /> : "刷新"}
        </button>
      </div>
      <div className="split content">
        <div className="list-col">
          <div className="history-head">
            <span>软件</span>
            <span>卸载时间</span>
            <span>卸载结果</span>
            <span style={{ textAlign: "right" }}>残留</span>
          </div>
          {records.map((rec) => (
            <div
              key={rec.id}
              className={"history-row" + (selectedId === rec.id ? " selected" : "")}
              onClick={() => setSelectedId(rec.id)}
            >
              <span className="name">{rec.app.displayName}</span>
              <span className="meta">{fmtTime(rec.uninstalledAt)}</span>
              <span className={"pill " + (rec.success ? "safe" : "warn")}>
                {rec.success ? "成功" : "未完成"}
              </span>
              <span className="meta" style={{ textAlign: "right" }}>
                {rec.residualCount} 项
              </span>
            </div>
          ))}
          {!loading && records.length === 0 && (
            <div className="muted" style={{ padding: 24 }}>
              还没有卸载记录。通过 CleanSweep 卸载软件后会自动记录在这里。
            </div>
          )}
        </div>
        <div className="detail-col">
          {selectedRecord ? (
            <>
              <div className="section">
                <div className="h">卸载记录</div>
                <div className="title">{selectedRecord.app.displayName}</div>
                <div className="sub">
                  {selectedRecord.app.publisher ?? "未知发布者"} ·{" "}
                  {selectedRecord.app.version ?? "—"}
                </div>
                <div className="history-kv">
                  <span className="k">卸载时间</span>
                  <span>{fmtTime(selectedRecord.uninstalledAt)}</span>
                  <span className="k">安装目录</span>
                  <span className="mono" style={{ wordBreak: "break-all" }}>
                    {selectedRecord.app.installLocation ?? "—"}
                  </span>
                  <span className="k">卸载结果</span>
                  <span>
                    {selectedRecord.success ? "成功" : "未完成"} · 退出码{" "}
                    {selectedRecord.exitCode ?? "—"}
                  </span>
                  <span className="k">最近扫描</span>
                  <span>{fmtTime(selectedRecord.lastScanAt)}</span>
                </div>
                {!selectedRecord.success && (
                  <div className="hint" style={{ marginBottom: 8 }}>
                    「未完成」只按卸载器自己的退出码判定（本次{" "}
                    {selectedRecord.exitCode ?? "—"}），不代表卸载没生效。是否还有残留以复查结果为准：{scanSummary}
                  </div>
                )}
                <div className="row">
                  <button className="primary" onClick={() => void scan()} disabled={scanBusy}>
                    {scanBusy ? <span className="spinner" /> : null} 重新扫描残留并 AI 判断
                  </button>
                  <button className="ghost" onClick={() => void removeRecord()}>
                    删除记录
                  </button>
                </div>
                {scanError && (
                  <div className="hint" style={{ color: "var(--warn)", marginTop: 8 }}>
                    {scanError}
                  </div>
                )}
              </div>

              <div className="section">
                <div className="h">残留审核</div>
                {scanBusy ? (
                  <div className="hint"><span className="spinner" /> 正在扫描残留…</div>
                ) : residuals.length === 0 ? (
                  <>
                    <div className="hint">
                      {aiGuess
                        ? "这一轮扫描没有找到残留项（安装目录、AppData、注册表、服务/任务都查过了）。"
                        : selectedRecord.lastScanAt
                          ? `上一次扫描（${fmtTime(selectedRecord.lastScanAt)}）没有找到残留项。固定规则只按名字匹配，可能漏项，建议用上方「重新扫描残留并 AI 判断」再查一遍。`
                          : "还没有扫描过。点上方「重新扫描残留并 AI 判断」查一遍（安装目录、AppData、注册表、服务/任务）。"}
                    </div>
                    {aiGuess?.error && (
                      <div className="hint" style={{ color: "var(--warn)", marginTop: 6 }}>
                        AI 推断没跑通，这轮结论只来自固定规则，可能漏报： {" "}
                        {aiGuess.error}
                      </div>
                    )}
                  </>
                ) : (
                  <>
                    {aiGuess && (aiGuess.summary || aiGuess.guessed > 0) && (
                      <div className="hint">
                        {aiGuess.summary}
                        {aiGuess.guessed > 0
                          ? `${aiGuess.summary ? " · " : ""}其中 ${aiGuess.guessed} 项来自 AI 推断`
                          : ""}
                      </div>
                    )}
                    {aiGuess?.error && (
                      <div className="hint" style={{ color: "var(--warn)" }}>
                        AI 推断未生效：{aiGuess.error}（下列结果只来自固定规则，可能漏报）
                      </div>
                    )}
                    <div className="row" style={{ marginBottom: 6 }}>
                      <span className="tag">AI 清理安全审核</span>
                      {aiReady === false && <span className="tag">未启用 AI</span>}
                      <div className="spacer" />
                      <button className="ghost" onClick={() => toggleAll(true)}>全选</button>
                      <button className="ghost" onClick={() => toggleAll(false)}>清空</button>
                    </div>
                    {aiReady === false && (
                      <div className="hint">未启用 AI：将只使用内置安全规则，请人工核对路径。</div>
                    )}
                    {aiReady !== false && aiBusy && (
                      <div className="hint">AI 正在逐项审核清理项…</div>
                    )}
                    {aiReady !== false && !aiBusy && !assessment && (
                      <div className="hint" style={{ color: "var(--warn)" }}>
                        AI 安全审核未完成，暂不能删除。请重新扫描或检查 AI 配置。
                      </div>
                    )}
                    {assessment && (
                      <div className="hint">
                        已选 {selectedItems.length} 项：可删{" "}
                        {selectedItems.filter((r) => verdictOf(r.id) === "safe").length} · 谨慎{" "}
                        {selectedItems.filter((r) => verdictOf(r.id) === "caution").length} · 保留{" "}
                        {keepItems.length} · 未判断{" "}
                        {selectedItems.filter((r) => !verdictOf(r.id)).length}
                      </div>
                    )}
                    {keepItems.length > 0 && (
                      <div className="hint" style={{ color: "var(--danger)" }}>
                        AI 建议保留部分选中项，请取消勾选后再删除。
                      </div>
                    )}
                    {keepItems.length === 0 && cautiousItems.length > 0 && (
                      <label className="checkbox" style={{ margin: "4px 0" }}>
                        <input
                          type="checkbox"
                          checked={riskOverride}
                          onChange={(e) => setRiskOverride(e.target.checked)}
                        />
                        我了解 AI 对部分选中项给出谨慎/未知结论，仍要删除
                      </label>
                    )}

                    {residuals.map((r) => {
                      const v = verdictOf(r.id);
                      const reason = reasonOf(r.id);
                      return (
                        <div key={r.id} className="history-res">
                          <input
                            type="checkbox"
                            checked={!!selected[r.id]}
                            onChange={(e) =>
                              setSelected((s) => ({ ...s, [r.id]: e.target.checked }))
                            }
                          />
                          <div>
                            <div className="path">{r.path}</div>
                            <div className="meta">
                              {r.display}
                              {r.detail ? ` · ${r.detail}` : ""}
                              {r.sizeBytes ? ` · ${fmtSize(r.sizeBytes)}` : ""}
                            </div>
                            {reason && <div className="meta">AI: {reason}</div>}
                          </div>
                          <span className="tag">{KIND_LABEL[r.kind] ?? r.kind}</span>
                          <span className={"pill " + verdictPill(v)}>{verdictLabel(v)}</span>
                        </div>
                      );
                    })}

                    <div className="actions" style={{ marginTop: 10 }}>
                      <button
                        className="danger"
                        disabled={deleteDisabled}
                        title={
                          keepItems.length > 0
                            ? "AI 建议保留部分选中项"
                            : aiGatePending
                              ? "AI 安全审核未完成"
                              : deleteNeedsOverride
                                ? "有谨慎/未知项，请先勾选风险确认"
                                : ""
                        }
                        onClick={() => void doDelete()}
                      >
                        删除选中 ({selectedItems.length})
                      </button>
                    </div>
                  </>
                )}
              </div>
            </>
          ) : (
            <div className="section">
              <div className="h">卸载历史</div>
              <div className="title">选择一条记录</div>
              <div className="sub">
                每次卸载都会记录软件信息、卸载结果与当时发现的残留。之后可以重新扫描残留，
                并让 AI 判断是否还有未清理干净的项目。
              </div>
            </div>
          )}
        </div>
      </div>
    </>
  );
}
