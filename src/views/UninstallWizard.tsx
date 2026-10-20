import { useEffect, useMemo, useState } from "react";
import {
  api,
  onEvent,
  type InstalledApp,
  type ResidualItem,
  type ResidualKind,
  type AiResidualAssessment,
  type AiSafetyVerdict,
  type AiUninstallRisk,
  type UninstallResult,
  type DeleteResult,
} from "../bindings";
import { useConfirm } from "../components/Confirm";

type Step = "intro" | "uninstalling" | "uninstalled" | "scanning" | "review" | "deleting" | "done";

interface CleanupGroup {
  key: string;
  label: string;
  items: ResidualItem[];
  bytes: number;
  locations: string[];
}

const CLEANUP_GROUP_ORDER = [
  "programfiles",
  "programdata",
  "localappdata",
  "roaming",
  "shortcut",
  "registry",
  "service",
  "task",
  "other",
];

/** 把残留项归类到用户能看懂的“目录/数据”分组。 */
function cleanupGroupOf(r: ResidualItem): { key: string; label: string } {
  switch (r.kind) {
    case "registrykey":
      return { key: "registry", label: "注册表项" };
    case "service":
      return { key: "service", label: "Windows 服务" };
    case "scheduledtask":
      return { key: "task", label: "计划任务" };
    case "shortcut":
      return { key: "shortcut", label: "开始菜单快捷方式" };
    default:
      break;
  }
  const p = r.path.toLowerCase().replace(/\//g, "\\");
  if (p.includes("\\programdata")) return { key: "programdata", label: "公共数据 (ProgramData)" };
  if (p.includes("\\appdata\\locallow")) return { key: "localappdata", label: "用户数据 (LocalLow)" };
  if (p.includes("\\appdata\\local")) return { key: "localappdata", label: "用户数据 (LocalAppData)" };
  if (p.includes("\\appdata\\roaming")) return { key: "roaming", label: "用户数据 (Roaming)" };
  if (p.includes("\\program files")) return { key: "programfiles", label: "程序目录 (Program Files)" };
  return {
    key: "other",
    label: r.kind === "directory" ? "其他目录" : "其他文件",
  };
}

/** 汇总本次扫描到的清理范围：分组、位置、项数与占用空间。 */
function summarizeCleanup(items: ResidualItem[]): CleanupGroup[] {
  const map = new Map<string, CleanupGroup>();
  for (const r of items) {
    const { key, label } = cleanupGroupOf(r);
    let group = map.get(key);
    if (!group) {
      group = { key, label, items: [], bytes: 0, locations: [] };
      map.set(key, group);
    }
    group.items.push(r);
    group.bytes += r.sizeBytes ?? 0;
    const loc =
      r.kind === "service" || r.kind === "scheduledtask" ? r.display : r.path;
    if (loc && !group.locations.includes(loc)) {
      group.locations.push(loc);
    }
  }
  return [...map.values()].sort(
    (a, b) => CLEANUP_GROUP_ORDER.indexOf(a.key) - CLEANUP_GROUP_ORDER.indexOf(b.key),
  );
}

/** 显示在“清理范围”里的位置摘要（最多显示两项，其余折叠）。 */
function locationsText(group: CleanupGroup): string {
  const shown = group.locations.slice(0, 2).join("；");
  return group.locations.length > 2
    ? `${shown} 等 ${group.locations.length} 处`
    : shown || "—";
}

function kindLabel(kind: ResidualKind): string {
  return {
    file: "文件",
    directory: "目录",
    registrykey: "注册表",
    shortcut: "快捷方式",
    service: "服务",
    scheduledtask: "计划任务",
    envvar: "环境变量",
  }[kind];
}

/** 常见的“静默/无界面”卸载参数（用于向用户提示：软件自带的卸载串可能本来就是静默的）。 */
const SILENT_FLAG_RE =
  /(?:^|\s)(?:\/S|--?silent|--?quiet|\/SILENT|\/VERYSILENT|\/QUIET|\/QN)\b/i;

function hasSilentFlags(command: string): boolean {
  return SILENT_FLAG_RE.test(command);
}

interface ResolvedUninstallCommand {
  command: string;
  quietAvailable: boolean;
  storeApp: boolean;
  usingQuiet: boolean;
}

/**
 * 计算本次实际会执行的卸载命令：
 * - Store 应用由后端调用 Remove-AppxPackage；
 * - 勾选静默且软件提供 QuietUninstallString 时用静默串；
 * - 否则使用注册表里的 UninstallString（可能本身就带 /SILENT，CleanSweep 不会修改它）。
 */
function resolveUninstallCommand(
  app: InstalledApp,
  silent: boolean,
): ResolvedUninstallCommand {
  const normal = (app.uninstallString ?? "").trim();
  const quiet = (app.quietUninstallString ?? "").trim();
  const storeApp = app.source === "store";
  if (storeApp) {
    return {
      command: normal || "Remove-AppxPackage（由 CleanSweep 调用）",
      quietAvailable: false,
      storeApp,
      usingQuiet: false,
    };
  }
  if (silent && quiet) {
    return { command: quiet, quietAvailable: true, storeApp, usingQuiet: true };
  }
  return {
    command: normal,
    quietAvailable: Boolean(quiet),
    storeApp,
    usingQuiet: false,
  };
}

function errText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}

function riskLabel(r: AiUninstallRisk["riskLevel"]) {
  return { low: "低", medium: "中", high: "高", critical: "极高" }[r];
}
function riskPill(r: AiUninstallRisk["riskLevel"]) {
  return r === "low" ? "safe" : r === "medium" ? "warn" : "danger";
}

export function UninstallWizard({
  app,
  onClose,
  onDone,
}: {
  app: InstalledApp;
  onClose: () => void;
  onDone: () => void;
}) {
  const [step, setStep] = useState<Step>("intro");
  const [silent, setSilent] = useState(false);
  const [uninstallRes, setUninstallRes] = useState<UninstallResult | null>(null);
  const [residuals, setResiduals] = useState<ResidualItem[]>([]);
  const [assessment, setAssessment] = useState<AiResidualAssessment | null>(null);
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [deleteResults, setDeleteResults] = useState<DeleteResult[]>([]);
  const [log, setLog] = useState<string[]>([]);
  const [aiBusy, setAiBusy] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);
  /** 卸载前 AI 风险评估 */
  const [preRisk, setPreRisk] = useState<AiUninstallRisk | null>(null);
  const [preRiskBusy, setPreRiskBusy] = useState(false);
  const [preRiskError, setPreRiskError] = useState<string | null>(null);
  const [preRiskSkipped, setPreRiskSkipped] = useState(false);
  const [riskAck, setRiskAck] = useState(false);
  /** 卸载前确认“已知晓扫描/清理范围”（具体条目会在扫描后逐项确认）。 */
  const [scopeAck, setScopeAck] = useState(false);
  /** 卸载/清理后的复查结果（是否还有残留 = 是否卸载干净） */
  const [postCheck, setPostCheck] = useState<ResidualItem[] | null>(null);
  const [postChecking, setPostChecking] = useState(false);
  const confirm = useConfirm();
  // 旧版后端可能不返回 isProtected，缺失一律按“不受保护”处理。
  const isProtected = Boolean(app.isProtected);
  const uninstallCommand = resolveUninstallCommand(app, silent);
  const commandHasSilentFlags = hasSilentFlags(uninstallCommand.command);

  const dangerousRisk = preRisk?.riskLevel === "high" || preRisk?.riskLevel === "critical";

  useEffect(() => {
    const unsubs: Array<() => void> = [];
    onEvent<string>("uninstall-log", (msg) => setLog((l) => [...l, msg])).then((u) =>
      unsubs.push(() => u())
    );
    return () => unsubs.forEach((f) => f());
  }, []);

  // 打开向导即做“卸载前 AI 风险检查”；失败/未启用时允许继续，但会明确提示。
  useEffect(() => {
    if (isProtected) return;
    let alive = true;
    setPreRiskBusy(true);
    setPreRiskError(null);
    api
      .aiAnalyzeRisk(app)
      .then((r) => {
        if (alive) setPreRisk(r);
      })
      .catch((e) => {
        if (alive) setPreRiskError(errText(e));
      })
      .finally(() => {
        if (alive) setPreRiskBusy(false);
      });
    return () => {
      alive = false;
    };
  }, [app, isProtected]);

  const startUninstall = async () => {
    // 防御性拦截：受保护条目即使被意外触发，也绝不进入卸载流程。
    if (isProtected) return;
    if (dangerousRisk && !riskAck) return;

    const ok = await confirm({
      title: `确认卸载 ${app.displayName}`,
      danger: true,
      message: (
        <div>
          <p>将运行以下卸载命令（来自软件注册信息）：</p>
          <pre>{uninstallCommand.command || "（注册表未提供卸载命令）"}</pre>
          <p className="muted">
            {uninstallCommand.storeApp
              ? "Store 应用将由 CleanSweep 调用 Remove-AppxPackage 卸载。"
              : `运行方式：${
                  uninstallCommand.usingQuiet
                    ? "静默（软件提供的 QuietUninstallString）"
                    : "按软件自带命令执行"
                }`}
            ，卸载完成后自动扫描残留并由 AI 判断是否卸载干净。
          </p>
          {commandHasSilentFlags && (
            <p className="hint" style={{ color: "var(--warn)" }}>
              注意：这条命令本身已包含静默参数（如 /SILENT）。即使取消勾选“优先使用静默卸载”，
              也可能不会弹出卸载界面——这是软件注册信息里的原始命令，CleanSweep 不会修改它。
            </p>
          )}
          {preRisk && (
            <p>
              <span className={"pill " + riskPill(preRisk.riskLevel)}>
                AI 风险：{riskLabel(preRisk.riskLevel)}
              </span>
              <span className="muted"> {preRisk.recommendation}</span>
            </p>
          )}
        </div>
      ),
      confirmLabel: "运行卸载",
      cancelLabel: "取消",
    });
    if (!ok) return;

    setStep("uninstalling");
    try {
      const res = await api.uninstallApp(app, silent);
      setUninstallRes(res);
      setStep("uninstalled");
    } catch (e) {
      setLog((l) => [...l, "错误: " + errText(e)]);
      setStep("uninstalled");
    }
  };

  const assessWithAi = async (items?: ResidualItem[]) => {
    const list = items ?? residuals;
    if (list.length === 0) return;
    setAiBusy(true);
    setAiError(null);
    try {
      // AI 结果仅作展示：模型输出不可信（可能被提示注入），绝不用来自动勾选待删项。
      setAssessment(await api.aiAssessResiduals(app, list));
    } catch (e) {
      setAssessment(null);
      setAiError(errText(e));
    } finally {
      setAiBusy(false);
    }
  };

  const doScan = async () => {
    setStep("scanning");
    try {
      const r = await api.scanResidualsFor(app);
      setResiduals(r);
      const init: Record<string, boolean> = {};
      r.forEach((it) => (init[it.id] = false));
      setSelected(init);
      setAssessment(null);
      setAiError(null);
      setStep("review");
      if (r.length > 0) void assessWithAi(r);
    } catch (e) {
      setLog((l) => [...l, "扫描错误: " + errText(e)]);
      setStep("review");
    }
  };

  const runPostCheck = async () => {
    setPostChecking(true);
    setPostCheck(null);
    try {
      setPostCheck(await api.scanResidualsFor(app));
    } catch {
      setPostCheck(null);
    } finally {
      setPostChecking(false);
    }
  };

  const doDelete = async () => {
    const picks = residuals.filter((r) => selected[r.id]);
    if (picks.length === 0) {
      setStep("done");
      setPostCheck(null);
      return;
    }
    const pickGroups = summarizeCleanup(picks);
    const pickBytes = picks.reduce((s, r) => s + (r.sizeBytes ?? 0), 0);
    const ok = await confirm({
      title: `确认删除 ${picks.length} 项残留`,
      danger: true,
      message: (
        <div>
          <p>
            将删除以下项目（不可恢复）：涉及 <strong>{pickGroups.length}</strong> 类位置、共{" "}
            <strong>{picks.length}</strong> 项
            {pickBytes > 0 ? `，约 ${fmtSize(pickBytes)}` : ""}。
          </p>
          <ul>
            {pickGroups.map((g) => (
              <li key={g.key}>
                {g.label} · {g.items.length} 项
                {g.bytes > 0 ? ` · ${fmtSize(g.bytes)}` : ""}
              </li>
            ))}
          </ul>
          <pre style={{ maxHeight: 240, overflow: "auto" }}>
            {picks.map((p) => `[${kindLabel(p.kind)}] ${p.path}`).join("\n")}
          </pre>
          {assessment && <p className="muted">AI 判定仅供参考，最终删除项以你勾选的为准。</p>}
        </div>
      ),
      confirmLabel: "全部删除",
      cancelLabel: "取消",
    });
    if (!ok) return;

    setStep("deleting");
    try {
      const res = await api.deleteResiduals(picks);
      setDeleteResults(res);
    } catch (e) {
      setLog((l) => [...l, "删除错误: " + errText(e)]);
    } finally {
      setStep("done");
      void runPostCheck();
    }
  };

  const selectAiSafe = () => {
    if (!assessment) return;
    const safe = new Set(
      assessment.perItem.filter((p) => p.verdict === "safe").map((p) => p.residualId)
    );
    setSelected((prev) => {
      const next = { ...prev };
      residuals.forEach((r) => {
        if (safe.has(r.id)) next[r.id] = true;
      });
      return next;
    });
  };

  const cleanAgain = () => {
    if (!postCheck || postCheck.length === 0) return;
    const init: Record<string, boolean> = {};
    postCheck.forEach((it) => (init[it.id] = false));
    setResiduals(postCheck);
    setSelected(init);
    setAssessment(null);
    setAiError(null);
    setPostCheck(null);
    setStep("review");
    void assessWithAi(postCheck);
  };

  const verdictOf = (id: string): AiSafetyVerdict | undefined =>
    assessment?.perItem.find((v) => v.residualId === id)?.verdict;

  const reasonOf = (id: string): string | undefined =>
    assessment?.perItem.find((v) => v.residualId === id)?.reason;

  const busyUninstalling = step === "uninstalling" || step === "deleting";
  // 卸载/删除进行中禁止关闭向导，否则会丢失进度与后续的残留复查。
  const requestClose = () => {
    if (busyUninstalling) return;
    onClose();
  };

  const bytesFreed = useMemo(() => {
    const freedIds = new Set(deleteResults.filter((d) => d.success).map((d) => d.id));
    return residuals
      .filter((r) => freedIds.has(r.id))
      .reduce((s, r) => s + (r.sizeBytes ?? 0), 0);
  }, [deleteResults, residuals]);

  // 本次扫描到、可能被清理的目录/数据分组（供用户确认“到底动哪里”）。
  const cleanupGroups = useMemo(() => summarizeCleanup(residuals), [residuals]);
  const cleanupBytes = useMemo(
    () => residuals.reduce((s, r) => s + (r.sizeBytes ?? 0), 0),
    [residuals],
  );
  const selectedCleanupCount = residuals.filter((r) => selected[r.id]).length;
  const groupAllSelected = (g: CleanupGroup) =>
    g.items.length > 0 && g.items.every((it) => Boolean(selected[it.id]));
  const toggleCleanupGroup = (g: CleanupGroup) => {
    setSelected((prev) => {
      const target = !g.items.every((it) => Boolean(prev[it.id]));
      const next = { ...prev };
      g.items.forEach((it) => (next[it.id] = target));
      return next;
    });
  };
  const selectAllCleanup = () => {
    setSelected(() => {
      const next: Record<string, boolean> = {};
      residuals.forEach((r) => (next[r.id] = true));
      return next;
    });
  };
  const clearAllCleanup = () => setSelected({});

  // 受保护条目：向导本身只呈现“已阻止”状态，不提供任何卸载入口。
  if (isProtected) {
    return (
      <div className="backdrop" onClick={onClose}>
        <div className="modal" style={{ width: 760 }} onClick={(e) => e.stopPropagation()}>
          <div className="modal-head">
            <span className="h-title">卸载向导 · {app.displayName}</span>
            <button className="ghost" onClick={onClose}>关闭</button>
          </div>
          <div className="modal-body">
            <div className="protected-note" role="alert">
              <div className="note-head">
                <span className="tag protected">受保护</span>
                <strong>无法卸载 {app.displayName}</strong>
              </div>
              <p className="reason">
                {app.protectedReason?.trim() ||
                  "该条目属于系统组件或系统依赖，CleanSweep 不允许卸载。"}
              </p>
              <p className="hint">为避免系统不可用，CleanSweep 已阻止卸载该类条目。</p>
              <div className="actions" style={{ marginTop: 14 }}>
                <button onClick={onClose}>关闭</button>
              </div>
            </div>
          </div>
        </div>
      </div>
    );
  }

  const startDisabled =
    (!preRiskSkipped && preRiskBusy) || (dangerousRisk && !riskAck) || !scopeAck;

  return (
    <div className="backdrop" onClick={requestClose}>
      <div className="modal" style={{ width: 820 }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span className="h-title">卸载向导 · {app.displayName}</span>
          <button
            className="ghost"
            onClick={requestClose}
            disabled={busyUninstalling}
            title={busyUninstalling ? "卸载/删除进行中，请等待完成" : "关闭"}
          >
            {busyUninstalling ? "处理中…" : "关闭"}
          </button>
        </div>
        <div className="modal-body">
          <Steps current={step} />
          {step === "intro" && (
            <>
              <p>
                该向导会先让 AI 评估卸载风险，再运行官方卸载程序；卸载后自动扫描残留、由 AI
                判断是否卸载干净，最后逐项确认删除。
              </p>

              {/* 卸载前 AI 风险检查 */}
              <div
                style={{
                  margin: "10px 0",
                  padding: "10px 12px",
                  border: "1px solid var(--hair)",
                  borderRadius: "var(--radius)",
                  background: "var(--panel-2)",
                }}
              >
                <div className="row" style={{ marginBottom: 6 }}>
                  <span className="tag">卸载前 AI 检查</span>
                  {preRiskBusy && <span className="spinner" />}
                  {preRisk && (
                    <span className={"pill " + riskPill(preRisk.riskLevel)}>
                      风险 {riskLabel(preRisk.riskLevel)}
                    </span>
                  )}
                </div>
                {preRiskBusy && (
                  <div className="hint">AI 正在分析卸载该软件的风险、依赖与受影响组件…</div>
                )}
                {!preRiskBusy && preRiskError && (
                  <div className="hint" style={{ color: "var(--warn)" }}>
                    AI 风险检查不可用：{preRiskError}
                    （不影响继续卸载，但将没有 AI 风险提示；可先到设置页配置/测试 AI。）
                  </div>
                )}
                {preRisk && (
                  <>
                    <p style={{ margin: "4px 0 6px" }}>{preRisk.recommendation}</p>
                    {preRisk.dependencies.length > 0 && (
                      <div className="hint">
                        <strong>可能依赖：</strong>
                        {preRisk.dependencies.join("、")}
                      </div>
                    )}
                    {preRisk.affectedComponents.length > 0 && (
                      <div className="hint" style={{ color: "var(--warn)" }}>
                        <strong>可能受影响：</strong>
                        {preRisk.affectedComponents.join("、")}
                      </div>
                    )}
                    {dangerousRisk && (
                      <label className="checkbox" style={{ marginTop: 8 }}>
                        <input
                          type="checkbox"
                          checked={riskAck}
                          onChange={(e) => setRiskAck(e.target.checked)}
                        />
                        我已了解上述高风险提示，仍要卸载
                      </label>
                    )}
                  </>
                )}
                {preRiskBusy && !preRiskSkipped && (
                  <button
                    className="ghost"
                    style={{ marginTop: 6 }}
                    onClick={() => setPreRiskSkipped(true)}
                  >
                    跳过等待，直接卸载（不推荐）
                  </button>
                )}
              </div>

              <details className="scope-details" open>
                <summary>本次会扫描 / 清理哪些目录与数据？</summary>
                <div className="scope-details-body">
                  <div className="scope-app">
                    <div>
                      <span className="scope-app-label">当前软件</span>
                      <strong>{app.displayName}</strong>
                    </div>
                    <div className="scope-app-path">
                      已知安装目录：
                      <code>{app.installLocation || "注册表未记录（以扫描结果为准）"}</code>
                    </div>
                  </div>
                  <ul>
                    <li>
                      <strong>程序目录：</strong>注册表记录的安装目录，以及 Program Files /
                      Program Files (x86) 下与应用相关的目录
                    </li>
                    <li>
                      <strong>用户与公共数据：</strong>LocalAppData、Roaming、ProgramData
                      下与应用相关的目录和文件
                    </li>
                    <li>
                      <strong>开始菜单：</strong>仅与应用相关的快捷方式（.lnk）
                    </li>
                    <li>
                      <strong>注册表：</strong>HKCU / HKLM\SOFTWARE 下与应用同名的专属键；
                      不会触碰 Microsoft\Windows 等系统项
                    </li>
                    <li>
                      <strong>服务与计划任务：</strong>仅非 Windows 系统服务、非 Windows
                      内置计划任务
                    </li>
                  </ul>
                  <p className="hint">
                    AI 分析只使用软件名、发布者、版本、安装目录、卸载串和残留清单，不会上传文件内容；
                    API Key 仅保存在本机。
                  </p>
                  <label className="checkbox scope-ack">
                    <input
                      type="checkbox"
                      checked={scopeAck}
                      onChange={(e) => setScopeAck(e.target.checked)}
                    />
                    我已了解上述扫描 / 清理范围；具体清理项会在扫描后逐条列出，由我勾选确认。
                  </label>
                </div>
              </details>

              <label className="checkbox" style={{ margin: "8px 0" }}>
                <input type="checkbox" checked={silent} onChange={(e) => setSilent(e.target.checked)} />
                优先使用软件提供的静默卸载命令（QuietUninstallString）
              </label>

              <div className="uninstall-command">
                <div className="uc-head">
                  <span>将要运行的卸载命令（来自软件注册信息）</span>
                  <span className="tag">
                    {commandHasSilentFlags
                      ? "自带静默参数"
                      : uninstallCommand.usingQuiet
                        ? "静默串"
                        : "软件自带串"}
                  </span>
                </div>
                <pre>{uninstallCommand.command || "（注册表未提供卸载命令）"}</pre>
                {commandHasSilentFlags && (
                  <div className="hint" style={{ color: "var(--warn)" }}>
                    该命令本身已经包含静默参数（如 /SILENT /VERYSILENT /QUIET）。即使取消勾选上面的选项，
                    也可能不会弹出卸载界面；CleanSweep 按软件注册信息原样执行，不会改写软件自带的卸载命令。
                  </div>
                )}
                {silent && !uninstallCommand.quietAvailable && (
                  <div className="hint">
                    该软件没有提供独立的 QuietUninstallString，勾选后仍会执行上面的命令。
                  </div>
                )}
                {!silent && !uninstallCommand.quietAvailable && !commandHasSilentFlags && (
                  <div className="hint">
                    未勾选静默卸载时，将按软件自带命令执行；若软件自身支持交互式界面，会正常弹出。
                  </div>
                )}
              </div>
              <div className="actions">
                <button onClick={onClose}>取消</button>
                <button
                  className="primary"
                  onClick={startUninstall}
                  disabled={startDisabled}
                  title={
                    dangerousRisk && !riskAck
                      ? "AI 判定为高风险，请先勾选“我已了解风险”"
                      : !scopeAck
                        ? "请先展开并勾选“我已了解扫描 / 清理范围”"
                        : preRiskBusy && !preRiskSkipped
                          ? "正在等待 AI 风险检查结果"
                          : ""
                  }
                >
                  {preRiskBusy && !preRiskSkipped ? (
                    <>
                      <span className="spinner" /> AI 检查中…
                    </>
                  ) : (
                    "开始卸载"
                  )}
                </button>
              </div>
            </>
          )}

          {step === "uninstalling" && (
            <div className="row">
              <span className="spinner" /> 正在运行卸载程序并等待结束（最长 30 分钟）
              {log.length > 0 && (
                <pre style={{ marginTop: 8, maxHeight: 200 }}>{log.join("\n")}</pre>
              )}
            </div>
          )}

          {step === "uninstalled" && (
            <>
              {uninstallRes && (
                <div className="row" style={{ marginBottom: 8 }}>
                  <span className={"pill " + (uninstallRes.success ? "safe" : "danger")}>
                    {uninstallRes.success ? "卸载程序已结束" : "卸载可能失败"}
                  </span>
                  {uninstallRes.requiresReboot && (
                    <span className="pill warn">建议重启后再扫描残留</span>
                  )}
                </div>
              )}
              <pre style={{ maxHeight: 200 }}>{uninstallRes?.log}</pre>
              <div className="hint">
                如果软件自带的卸载器窗口/进度条还没有结束，请先等它完成，再点击「扫描残留」；
                CleanSweep 会等待应用进程和安装记录消失，但个别卸载器可能超出等待时间。
              </div>
              <div className="actions">
                <button onClick={onClose}>稍后再清理</button>
                <button className="primary" onClick={doScan}>
                  扫描残留并让 AI 判断是否干净
                </button>
              </div>
            </>
          )}

          {step === "scanning" && (
            <div className="row"><span className="spinner" /> 扫描残留中…</div>
          )}

          {step === "review" && (
            <>
              <div className="row" style={{ marginBottom: 8 }}>
                <span className={residuals.length === 0 ? "pill safe" : "pill warn"}>
                  {residuals.length === 0 ? "未发现残留" : `发现 ${residuals.length} 项残留`}
                </span>
                {residuals.length > 0 && (
                  <span className="muted" style={{ fontSize: 11 }}>
                    残留越少说明卸载越干净
                  </span>
                )}
                <div className="spacer" />
                {residuals.length > 0 && (
                  <>
                    <button
                      onClick={() => void assessWithAi()}
                      disabled={aiBusy}
                      title="AI 结论仅供参考，不会自动勾选任何项目"
                    >
                      {aiBusy ? <span className="spinner" /> : null} 重新 AI 评估
                    </button>
                    <button
                      onClick={selectAiSafe}
                      disabled={!assessment || aiBusy}
                      title="点击后只勾选 AI 判为“可删”的项目，仍会弹确认框"
                    >
                      勾选 AI 建议可删项
                    </button>
                  </>
                )}
              </div>

              {residuals.length > 0 && aiBusy && (
                <div className="hint" style={{ marginBottom: 8 }}>
                  AI 正在分析残留项，判断是否卸载干净…
                </div>
              )}
              {aiError && (
                <div className="hint" style={{ marginBottom: 8, color: "var(--warn)" }}>
                  AI 分析失败：{aiError}（仍可手动勾选删除；可先到设置页检查 AI 配置）
                </div>
              )}
              {assessment && (
                <div className="row" style={{ marginBottom: 8 }}>
                  <span className={"pill " + overallPill(assessment.overall)}>
                    AI 判断：{overallLabel(assessment.overall)}
                  </span>
                  <span className="muted" style={{ fontSize: 11 }}>
                    AI 不会自动勾选，最终删除项由你选择。
                  </span>
                </div>
              )}

              {residuals.length > 0 && (
                <div className="cleanup-scope" aria-label="本次清理范围">
                  <div className="scope-head">
                    <span className="scope-title">本次将清理的目录与数据</span>
                    <span className="scope-total">
                      已选 {selectedCleanupCount}/{residuals.length} 项 ·{" "}
                      {cleanupBytes > 0 ? `约 ${fmtSize(cleanupBytes)}` : "大小未知"}
                    </span>
                    <span className="scope-actions">
                      <button type="button" className="ghost" onClick={selectAllCleanup}>
                        全选
                      </button>
                      <button type="button" className="ghost" onClick={clearAllCleanup}>
                        清空
                      </button>
                    </span>
                  </div>
                  {cleanupGroups.map((g) => (
                    <div className="scope-row" key={g.key}>
                      <input
                        type="checkbox"
                        className="scope-group-check"
                        checked={groupAllSelected(g)}
                        onChange={() => toggleCleanupGroup(g)}
                        title="选中 / 取消该类的全部项目"
                      />
                      <span className="scope-label">{g.label}</span>
                      <span className="scope-paths" title={g.locations.join("\n")}>
                        {locationsText(g)}
                      </span>
                      <span className="scope-count">{g.items.length} 项</span>
                      <span className="scope-size">{g.bytes > 0 ? fmtSize(g.bytes) : "—"}</span>
                    </div>
                  ))}
                  <div className="scope-foot">
                    仅显示本次扫描到的应用专属残留；Windows 系统目录、关键服务与 Windows
                    内置计划任务不会出现在这里。可先按类别全选，再在下方逐项核对/取消。
                  </div>
                </div>
              )}

              {residuals.length === 0 ? (
                <div
                  style={{
                    padding: "12px 14px",
                    border: "1px solid #2a4a36",
                    background: "var(--safe-soft)",
                    borderRadius: "var(--radius)",
                  }}
                >
                  <strong>卸载干净：未发现残留。</strong>
                  <div className="hint">
                    已扫描安装目录、应用数据、开始菜单、注册表、服务与计划任务，全部没有发现。
                  </div>
                </div>
              ) : (
                <div>
                  {residuals.map((r) => {
                    const v = verdictOf(r.id);
                    const reason = reasonOf(r.id);
                    return (
                      <div key={r.id} className="res-row">
                        <input
                          type="checkbox"
                          checked={!!selected[r.id]}
                          onChange={(e) => setSelected((s) => ({ ...s, [r.id]: e.target.checked }))}
                        />
                        <div>
                          <div className="path">{r.path}</div>
                          <div className="meta">
                            {r.display}{r.detail ? ` · ${r.detail}` : ""}
                            {r.sizeBytes ? ` · ${fmtSize(r.sizeBytes)}` : ""}
                            {r.inUse ? " · 占用中" : ""}
                          </div>
                          {reason && <div className="meta">AI: {reason}</div>}
                        </div>
                        <span className="tag">{r.kind}</span>
                        <span className={"pill " + verdictPill(v)}>{v ? verdictLabel(v) : "—"}</span>
                        <button className="ghost" onClick={() => api.openInExplorer(r.path)} title="打开所在位置">↗</button>
                      </div>
                    );
                  })}
                </div>
              )}
              <div className="actions" style={{ marginTop: 10 }}>
                {residuals.length === 0 ? (
                  <button className="primary" onClick={onDone}>完成</button>
                ) : (
                  <>
                    <button onClick={onClose}>保留剩余</button>
                    <button
                      className="danger"
                      disabled={!Object.values(selected).some(Boolean)}
                      onClick={doDelete}
                    >
                      删除选中 ({Object.values(selected).filter(Boolean).length})
                    </button>
                  </>
                )}
              </div>
            </>
          )}

          {step === "deleting" && (
            <div className="row"><span className="spinner" /> 正在删除选中残留</div>
          )}

          {step === "done" && (
            <>
              {bytesFreed > 0 && (
                <div className="hero">
                  <div className="big">{fmtSize(bytesFreed)}</div>
                  <div className="label">已释放空间</div>
                </div>
              )}
              {deleteResults.length > 0 && (
                <div>
                  {deleteResults.map((r) => (
                    <div
                      key={r.id}
                      style={{
                        display: "flex",
                        gap: 10,
                        alignItems: "flex-start",
                        padding: "7px 0",
                        borderBottom: "1px solid var(--hair)",
                      }}
                    >
                      <span className={"pill " + (r.success ? "safe" : "danger")}>
                        {r.success ? "已删" : "失败"}
                      </span>
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div className="mono" style={{ fontSize: 11, wordBreak: "break-all" }}>
                          {r.id}
                        </div>
                        <div className="muted" style={{ fontSize: 10, marginTop: 2 }}>
                          {r.message}
                        </div>
                      </div>
                    </div>
                  ))}
                </div>
              )}

              {/* 复查：是否还残留（是否卸载干净） */}
              <div
                style={{
                  marginTop: 12,
                  padding: "10px 12px",
                  border: "1px solid var(--hair)",
                  borderRadius: "var(--radius)",
                  background: "var(--panel-2)",
                }}
              >
                <div className="row">
                  <span className="tag">卸载干净复查</span>
                  {postChecking && <span className="spinner" />}
                  {!postChecking && postCheck && postCheck.length === 0 && (
                    <span className="pill safe">已卸载干净</span>
                  )}
                  {!postChecking && postCheck && postCheck.length > 0 && (
                    <span className="pill warn">仍有 {postCheck.length} 项残留</span>
                  )}
                </div>
                {postChecking && (
                  <div className="hint">正在复查残留（安装目录、AppData、注册表、服务、计划任务）…</div>
                )}
                {!postChecking && postCheck && postCheck.length === 0 && (
                  <div className="hint">复查未发现残留，卸载干净。</div>
                )}
                {!postChecking && postCheck && postCheck.length > 0 && (
                  <>
                    <div className="hint">
                      可能被占用、需要重启，或需要管理员权限；可再次清理剩余项。
                    </div>
                    <button style={{ marginTop: 6 }} onClick={cleanAgain}>
                      再次清理剩余
                    </button>
                  </>
                )}
                {!postChecking && postCheck === null && deleteResults.length > 0 && (
                  <div className="hint">复查未完成（后端未返回结果），可关闭后重新打开向导扫描。</div>
                )}
              </div>

              <div className="actions" style={{ marginTop: 12 }}>
                <button className="primary" onClick={onDone}>完成</button>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

function Steps({ current }: { current: Step }) {
  const steps: Step[] = ["intro", "uninstalling", "scanning", "review", "done"];
  const labels: Record<Step, string> = {
    intro: "AI 风险检查",
    uninstalling: "卸载",
    uninstalled: "卸载",
    scanning: "扫描残留",
    review: "AI 判断是否干净",
    deleting: "删除",
    done: "复查",
  };
  // 把过渡状态映射到所属阶段，避免步骤条在“卸载完成/删除中”时跳回第一步。
  const stage: Step =
    current === "uninstalled" ? "uninstalling" : current === "deleting" ? "review" : current;
  const idx = Math.max(0, steps.indexOf(stage));
  return (
    <div className="steps">
      {steps.map((s, i) => (
        <span key={s} className={"step" + (i < idx ? " done" : i === idx ? " now" : "")}>
          {labels[s]}
        </span>
      ))}
    </div>
  );
}

function fmtSize(b: number) {
  if (b >= 1_000_000_000) return (b / 1_000_000_000).toFixed(2) + " GB";
  if (b >= 1_000_000) return (b / 1_000_000).toFixed(1) + " MB";
  if (b >= 1000) return (b / 1000).toFixed(0) + " KB";
  return b + " B";
}
function verdictLabel(v: AiSafetyVerdict) {
  return { safe: "可删", caution: "谨慎", keep: "保留", unknown: "未知" }[v];
}
function verdictPill(v?: AiSafetyVerdict) {
  if (!v) return "";
  return v === "safe" ? "safe" : v === "caution" ? "warn" : v === "keep" ? "danger" : "";
}
function overallLabel(v: AiSafetyVerdict) {
  return { safe: "整体可放心清理", caution: "部分需谨慎", keep: "不建议清理", unknown: "无法判断" }[v];
}
function overallPill(v: AiSafetyVerdict) {
  return v === "safe" ? "safe" : v === "caution" ? "warn" : v === "keep" ? "danger" : "";
}
