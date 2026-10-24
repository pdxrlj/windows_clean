import { useEffect, useMemo, useState } from "react";
import {
  api,
  onEvent,
  type AiCleanliness,
  type AiResidualAssessment,
  type AiSafetyVerdict,
  type AiUninstallRisk,
  type DeleteResult,
  type ForceRemoveOutcome,
  type InstalledApp,
  type ResidualItem,
  type ResidualKind,
  type UninstallResult,
} from "../bindings";
import { useConfirm } from "../components/Confirm";

/**
 * 三步极简流程：
 * 1. 确认：只检查 AI 是否可用（不可用则阻止），显示卸载命令。
 * 2. 卸载与 AI 分析：卸载 → 等待 → 扫描 → AI 逐项判断，全部自动串行。
 * 3. 清理与复查：AI 建议项默认勾选，确认后删除，同页显示复查结果。
 *
 * 安全不依赖 AI：AI 可以推断固定规则漏掉的位置，但每一条都必须过后端五道校验
 * （真实存在 / 安全路径 / 应用数据根 / 名字相关 / 不属于其他软件），
 * 真正删除仍由索引校验 + 路径安全校验双重把关。
 */
type Step = "intro" | "working" | "review" | "done";
type WorkStage = "uninstalling" | "scanning" | "assessing" | "deleting" | "postcheck";
type AiGate = "checking" | "ready" | "missing" | "error";

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
    case "registryvalue":
      return { key: "registry", label: "启动项 / 注册表值" };
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
    registryvalue: "启动项",
    shortcut: "快捷方式",
    service: "服务",
    scheduledtask: "计划任务",
    envvar: "环境变量",
  }[kind];
}

/** 常见的“静默/无界面”卸载参数（用于提示：软件自带的卸载串可能本来就是静默的）。 */
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
  const confirm = useConfirm();
  // 旧版后端可能不返回 isProtected，缺失一律按“不受保护”处理。
  const isProtected = Boolean(app.isProtected);

  const [step, setStep] = useState<Step>("intro");
  const [stage, setStage] = useState<WorkStage>("uninstalling");
  const [silent, setSilent] = useState(false);

  /** AI 可用性检查：未配置时整个向导被阻止（不做“跳过 AI”的降级）。 */
  const [aiGate, setAiGate] = useState<AiGate>("checking");
  const [aiGateError, setAiGateError] = useState<string | null>(null);
  /** 卸载风险摘要：后台获取，仅作提示，不阻塞流程。 */
  const [preRisk, setPreRisk] = useState<AiUninstallRisk | null>(null);
  const [riskAck, setRiskAck] = useState(false);

  const [uninstallRes, setUninstallRes] = useState<UninstallResult | null>(null);
  const [residuals, setResiduals] = useState<ResidualItem[]>([]);
  const [assessment, setAssessment] = useState<AiResidualAssessment | null>(null);
  const [assessError, setAssessError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [deleteResults, setDeleteResults] = useState<DeleteResult[]>([]);
  const [postCheck, setPostCheck] = useState<ResidualItem[] | null>(null);
  const [log, setLog] = useState<string[]>([]);
  const [showAll, setShowAll] = useState(false);
  const [postChecking, setPostChecking] = useState(false);
  /** 卸载后的 AI 复查结论：这款软件算不算卸载干净。 */
  const [cleanReport, setCleanReport] = useState<AiCleanliness | null>(null);
  const [cleanReportError, setCleanReportError] = useState<string | null>(null);
  const [cleanChecking, setCleanChecking] = useState(false);
  /** AI 推断补充位置的统计（只用于展示，删除仍走后端校验）。 */
  const [aiGuess, setAiGuess] = useState<{
    guessed: number;
    summary: string;
    error: string | null;
  }>({ guessed: 0, summary: "", error: null });

  /** 强制删除模式：软件没有可用的卸载程序，不跑卸载器，直接删残留。 */
  const [forceMode, setForceMode] = useState(false);
  /** 强制删除时是否顺手删掉注册表里的卸载记录（它会一直挂在程序列表里）。 */
  const [removeEntry, setRemoveEntry] = useState(true);
  const [forceOutcome, setForceOutcome] = useState<ForceRemoveOutcome | null>(null);
  /** AI 推断重试中（超时后用户可以手动重跑）。 */
  const [suggestRetrying, setSuggestRetrying] = useState(false);

  // 逐项清单默认先给前 12 项，避免一次刷出几十行。
  const LIST_PREVIEW = 12;

  const command = resolveUninstallCommand(app, silent);
  const commandHasSilentFlags = hasSilentFlags(command.command);
  // 注册表里没有卸载命令：只能强制删除，不能去跑一个不存在的卸载器。
  const noUninstaller = !command.storeApp && command.command.trim().length === 0;
  const dangerousRisk =
    preRisk?.riskLevel === "high" ||
    preRisk?.riskLevel === "critical" ||
    preRisk?.safeToUninstall === false;

  const checkAi = async () => {
    setAiGate("checking");
    setAiGateError(null);
    try {
      const eff = await api.resolveAiEffective();
      if (!eff.ready) {
        setAiGate("missing");
        return;
      }
      setAiGate("ready");
      // 风险摘要失败不影响卸载；清理阶段的 AI 审核是硬要求。
      api.aiAnalyzeRisk(app).then(setPreRisk).catch(() => {});
    } catch (e) {
      setAiGate("error");
      setAiGateError(errText(e));
    }
  };

  useEffect(() => {
    if (isProtected) return;
    void checkAi();
    // 只在切换到另一款软件时重新检查。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [app.id, isProtected]);

  useEffect(() => {
    let un: (() => void) | undefined;
    onEvent<string>("uninstall-log", (msg) => setLog((l) => [...l, msg])).then((u) => {
      un = () => u();
    });
    return () => un?.();
  }, []);

  const verdictOf = (id: string): AiSafetyVerdict | undefined =>
    assessment?.perItem.find((p) => p.residualId === id)?.verdict;
  const reasonOf = (id: string): string | undefined =>
    assessment?.perItem.find((p) => p.residualId === id)?.reason;

  /** AI 判定只决定“默认勾选”，永远不能勾选 keep 项。 */
  const applyDefaultSelection = (items: ResidualItem[], a: AiResidualAssessment | null) => {
    const init: Record<string, boolean> = {};
    items.forEach((it) => {
      const v = a?.perItem.find((p) => p.residualId === it.id)?.verdict;
      init[it.id] = v === "safe";
    });
    setSelected(init);
  };

  const assess = async (items: ResidualItem[]): Promise<AiResidualAssessment | null> => {
    setStage("assessing");
    try {
      const a = await api.aiAssessResiduals(app, items);
      setAssessment(a);
      setAssessError(null);
      return a;
    } catch (e) {
      setAssessment(null);
      setAssessError(errText(e));
      return null;
    }
  };

  const scanAndAssess = async () => {
    setStage("scanning");
    let items: ResidualItem[] = [];
    let info: { guessed: number; summary: string; error: string | null } = {
      guessed: 0,
      summary: "",
      error: null,
    };
    try {
      // 规则扫描 + 让 AI 推断“这个软件还会留在哪”。
      // AI 只提供候选，后端会逐条校验存在性/安全路径/名字相关性。
      const r = await api.aiSuggestResiduals(app);
      items = r.items;
      info = { guessed: r.aiGuessed, summary: r.aiSummary, error: r.aiError ?? null };
    } catch (e) {
      const msg = errText(e);
      setLog((l) => [...l, "AI 推断不可用: " + msg]);
      // 命令本身报错（配置缺失、后端拒绝等）也要把原因带出去，否则用户看不到“重新推断”入口。
      info = { guessed: 0, summary: "", error: msg };
      try {
        items = await api.scanResidualsFor(app);
      } catch (e2) {
        setLog((l) => [...l, "扫描错误: " + errText(e2)]);
      }
    }
    setAiGuess(info);
    setResiduals(items);
    const a = items.length > 0 ? await assess(items) : null;
    applyDefaultSelection(items, a);
    setStep("review");
  };

  /**
   * AI 推断超时/失败后的重试：只重跑“扫描 + 推断 + 分析”，
   * 并保留用户已经手动改过的勾选。
   */
  const retrySuggest = async () => {
    if (suggestRetrying) return;
    setSuggestRetrying(true);
    setStage("scanning");
    try {
      const r = await api.aiSuggestResiduals(app);
      setResiduals(r.items);
      setAiGuess({ guessed: r.aiGuessed, summary: r.aiSummary, error: r.aiError ?? null });
      const a = r.items.length > 0 ? await assess(r.items) : null;
      setSelected((prev) => {
        const keep = new Set(
          (a?.perItem ?? []).filter((p) => p.verdict === "keep").map((p) => p.residualId),
        );
        const next: Record<string, boolean> = {};
        r.items.forEach((it) => {
          if (keep.has(it.id)) {
            next[it.id] = false;
            return;
          }
          const suggested = a?.perItem.find((p) => p.residualId === it.id)?.verdict === "safe";
          // 之前已经展示过的项，尊重用户的选择；新出现的项才用 AI 默认值。
          next[it.id] = it.id in prev ? prev[it.id] : suggested;
        });
        return next;
      });
    } catch (e) {
      setAiGuess((g) => ({ ...g, error: errText(e) }));
    } finally {
      setSuggestRetrying(false);
    }
  };

  /**
   * 强制删除：绕过自带卸载器，直接扫描并删除文件与注册表。
   * 用于卸载器缺失/损坏、卸载器跑不起来的软件。
   */
  const startForce = async () => {
    const ok = await confirm({
      title: `强制删除 ${app.displayName}`,
      danger: true,
      message: (
        <div>
          <p>
            不会调用软件自带的卸载程序（注册表里没有可用命令，或卸载器已丢失/损坏）。
            CleanSweep 将直接扫描并删除它的文件与注册表项，
            <strong>删除前会尝试创建系统还原点</strong>。
          </p>
          <p className="hint">
            强制删除不会执行软件自己的清理步骤：可能漏掉它自定义的位置，也可能删掉你仍需要的配置。
            出问题只能靠还原点恢复。
          </p>
        </div>
      ),
      confirmLabel: "确认强制删除",
      cancelLabel: "取消",
    });
    if (!ok) return;

    setForceMode(true);
    setForceOutcome(null);
    setUninstallRes(null);
    setAssessment(null);
    setAssessError(null);
    setDeleteResults([]);
    setPostCheck(null);
    setCleanReport(null);
    setCleanReportError(null);
    setStep("working");
    setStage("scanning");
    await scanAndAssess();
  };

  const start = async () => {
    if (isProtected || aiGate !== "ready") return;
    if (dangerousRisk && !riskAck) return;

    // 没有卸载程序：直接走强制删除，不去跑一个不存在的命令。
    if (noUninstaller) {
      await startForce();
      return;
    }

    const ok = await confirm({
      title: `卸载 ${app.displayName} 并清理残留`,
      danger: true,
      message: (
        <div>
          <p>将运行软件自带的卸载程序，然后自动扫描并用 AI 列出建议清理的残留。</p>
          <p className="muted">
            运行方式：
            {command.storeApp
              ? "Remove-AppxPackage"
              : command.usingQuiet
                ? "静默（QuietUninstallString）"
                : "软件自带命令"}
          </p>
          <pre>{command.command || "（注册表未提供卸载命令）"}</pre>
          {preRisk && (
            <p>
              <span className={"pill " + riskPill(preRisk.riskLevel)}>
                AI 风险：{riskLabel(preRisk.riskLevel)}
              </span>
              <span className="muted"> {preRisk.recommendation}</span>
            </p>
          )}
          <p className="hint">
            系统目录、Windows 关键服务与 Microsoft 注册表项由内置安全规则硬拦截，AI 无法放行。
          </p>
        </div>
      ),
      confirmLabel: "开始卸载",
      cancelLabel: "取消",
    });
    if (!ok) return;

    setStep("working");
    setStage("uninstalling");
    setUninstallRes(null);
    setAssessment(null);
    setAssessError(null);
    setDeleteResults([]);
    setPostCheck(null);

    let res: UninstallResult;
    try {
      res = await api.uninstallApp(app, silent);
      setUninstallRes(res);
    } catch (e) {
      setLog((l) => [...l, "卸载错误: " + errText(e)]);
      setResiduals([]);
      setSelected({});
      setStep("review");
      return;
    }
    if (!res.success) {
      // 卸载器可能仍在后台删文件：不自动扫描，交由用户决定何时扫描。
      setResiduals([]);
      setSelected({});
      setStep("review");
      return;
    }
    await scanAndAssess();
  };

  const manualScan = async () => {
    await scanAndAssess();
  };

  const retryAssess = async () => {
    if (residuals.length === 0) return;
    const a = await assess(residuals);
    applyDefaultSelection(residuals, a);
  };

  const picks = useMemo(() => {
    const keep = new Set(
      (assessment?.perItem ?? [])
        .filter((p) => p.verdict === "keep")
        .map((p) => p.residualId),
    );
    return residuals.filter((r) => selected[r.id] && !keep.has(r.id));
  }, [residuals, selected, assessment]);

  const picksBytes = useMemo(
    () => picks.reduce((s, r) => s + (r.sizeBytes ?? 0), 0),
    [picks],
  );
  const picksUnsafeCount = picks.filter((p) => verdictOf(p.id) !== "safe").length;

  const keepCount = (assessment?.perItem ?? []).filter((p) => p.verdict === "keep").length;
  const cautionCount = (assessment?.perItem ?? []).filter(
    (p) => p.verdict === "caution" || p.verdict === "unknown",
  ).length;
  const cleanupGroups = useMemo(() => summarizeCleanup(residuals), [residuals]);
  const scanBytes = useMemo(
    () => residuals.reduce((s, r) => s + (r.sizeBytes ?? 0), 0),
    [residuals],
  );
  const bytesFreed = useMemo(() => {
    const freed = new Set(deleteResults.filter((d) => d.success).map((d) => d.id));
    return residuals.filter((r) => freed.has(r.id)).reduce((s, r) => s + (r.sizeBytes ?? 0), 0);
  }, [deleteResults, residuals]);

  const toggleItem = (id: string) => {
    if (verdictOf(id) === "keep") return;
    setSelected((s) => ({ ...s, [id]: !s[id] }));
  };
  const toggleGroup = (g: CleanupGroup) => {
    const target = !g.items.every((it) => Boolean(selected[it.id]));
    setSelected((s) => {
      const next = { ...s };
      g.items.forEach((it) => {
        if (verdictOf(it.id) !== "keep") next[it.id] = target;
      });
      return next;
    });
  };
  const selectRecommended = () => applyDefaultSelection(residuals, assessment);
  const clearAll = () => setSelected({});

  const doClean = async () => {
    if (forceMode) {
      // 强制删除允许“只删卸载记录”而没有任何节点残留。
      if (picks.length === 0 && !removeEntry) return;
    } else if (picks.length === 0 || !assessment) {
      return;
    }
    const groups = summarizeCleanup(picks);
    const onlyEntry = forceMode && picks.length === 0;
    const ok = await confirm({
      title: onlyEntry ? "删除注册表里的卸载记录" : `确认清理 ${picks.length} 项残留`,
      danger: true,
      message: (
        <div>
          {onlyEntry ? (
            <p>没有选中任何残留项，只删除注册表里的卸载记录。删除后它会从程序列表里消失。</p>
          ) : (
            <p>
              涉及 <strong>{groups.length}</strong> 类位置，共 <strong>{picks.length}</strong> 项
              {picksBytes > 0 ? `，约 ${fmtSize(picksBytes)}` : ""}。删除后不可恢复。
            </p>
          )}
          <ul>
            {groups.map((g) => (
              <li key={g.key}>
                {g.label} · {g.items.length} 项
                {g.bytes > 0 ? ` · ${fmtSize(g.bytes)}` : ""}
              </li>
            ))}
          </ul>
          <pre style={{ maxHeight: 240, overflow: "auto" }}>
            {picks.map((p) => `[${kindLabel(p.kind)}] ${p.path}`).join("\n")}
          </pre>
          {picksUnsafeCount > 0 && (
            <p className="hint" style={{ color: "var(--warn)" }}>
              其中 {picksUnsafeCount} 项被 AI 标记为「谨慎 / 无法判断」，你已手动勾选。
            </p>
          )}
          <p className="hint">
            删除前仍会再次校验路径：系统目录、Windows 关键服务、Microsoft 注册表项一律拒绝。
          </p>
          {forceMode && (
            <p className="hint">
              强制删除：不运行卸载器；后端会先尝试创建系统还原点，再逐项删除。
              {removeEntry ? "同时删除注册表里的卸载记录。" : ""}
            </p>
          )}
        </div>
      ),
      confirmLabel: "确认清理",
      cancelLabel: "取消",
    });
    if (!ok) return;

    setStep("working");
    setStage("deleting");
    let results: DeleteResult[] = [];
    if (forceMode) {
      // 后端一次完成：建还原点 → 逐项删除 → （可选）删卸载记录。
      try {
        const out = await api.forceRemoveApp(app, picks, removeEntry);
        setForceOutcome(out);
        results = out.results;
      } catch (e) {
        setLog((l) => [...l, "强制删除错误: " + errText(e)]);
      }
    } else {
      try {
        results = await api.deleteResiduals(picks);
      } catch (e) {
        setLog((l) => [...l, "删除错误: " + errText(e)]);
      }
    }
    setDeleteResults(results);

    setStage("postcheck");
    setPostChecking(true);
    let left: ResidualItem[] | null = null;
    try {
      left = await api.scanResidualsFor(app);
    } catch {
      left = null;
    }
    setPostCheck(left);
    // 只有还有剩余项时才值得花一次 AI 调用；没剩余就是干净。
    if (left && left.length > 0) {
      setCleanChecking(true);
      try {
        const cleaned = results.filter((r) => r.success).length;
        setCleanReport(await api.aiVerifyClean(app, cleaned, left));
        setCleanReportError(null);
      } catch (e) {
        setCleanReport(null);
        setCleanReportError(errText(e));
      } finally {
        setCleanChecking(false);
      }
    }
    setPostChecking(false);
    setStep("done");
  };

  const cleanAgain = async () => {
    if (!postCheck || postCheck.length === 0) return;
    setResiduals(postCheck);
    setPostCheck(null);
    setDeleteResults([]);
    setCleanReport(null);
    setCleanReportError(null);
    setShowAll(false);
    setStep("review");
    const a = await assess(postCheck);
    applyDefaultSelection(postCheck, a);
  };

  const goSettings = () => {
    window.dispatchEvent(new Event("go-settings"));
    onClose();
  };

  // 受保护条目：向导只呈现“已阻止”状态，不提供任何卸载入口。
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
              <div className="actions" style={{ marginTop: 14 }}>
                <button onClick={onClose}>关闭</button>
              </div>
            </div>
          </div>
        </div>
      </div>
    );
  }

  const busy = step === "working";
  const uninstallOk = uninstallRes?.success === true;
  const canStart = aiGate === "ready" && (!dangerousRisk || riskAck);
  const deleteDisabled = picks.length === 0 || !assessment;
  const successCount = deleteResults.filter((r) => r.success).length;

  /** 复查结论：无剩余项直接算干净；有剩余项用 AI 的判定，拿不到就不下结论。 */
  const leftoverItems = postCheck ?? [];
  const cleanVerdict =
    postCheck && postCheck.length === 0
      ? { clean: true, summary: "复查未发现残留，软件已卸载干净。", nextStep: "", byAi: false }
      : cleanReport
        ? {
            clean: cleanReport.clean ?? cleanReport.verdict === "safe",
            summary: cleanReport.summary,
            nextStep: cleanReport.nextStep,
            byAi: true,
          }
        : null;
  const leftoverVerdictOf = (id: string) =>
    cleanReport?.perItem.find((p) => p.residualId === id);

  return (
    <div className="backdrop" onClick={() => !busy && onClose()}>
      <div className="modal" style={{ width: 820 }} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span className="h-title">卸载向导 · {app.displayName}</span>
          <button
            className="ghost"
            onClick={onClose}
            disabled={busy}
            title={busy ? "处理中，请等待完成" : "关闭"}
          >
            {busy ? "处理中…" : "关闭"}
          </button>
        </div>
        <div className="modal-body">
          <Steps current={step} />

          {step === "intro" && (
            <>
              <p className="hint" style={{ marginTop: 0 }}>
                {noUninstaller
                  ? "这款软件没有可用的卸载程序 → 直接扫描并删除它的文件与注册表，删除前尝试创建系统还原点。"
                  : "运行官方卸载程序 → 固定规则扫描 + AI 推断可能漏掉的位置 → AI 逐项判断该不该删，你确认后清理。"}
              </p>

              <div className="uninstall-command" style={{ marginTop: 8 }}>
                <div className="uc-head">
                  <span>当前软件</span>
                  <span className="tag">{app.version || "版本未知"}</span>
                </div>
                <div style={{ fontSize: 12, marginBottom: 2 }}>{app.displayName}</div>
                <div
                  className="hint"
                  style={{ fontFamily: "var(--font-mono)", wordBreak: "break-all", margin: 0 }}
                >
                  {app.installLocation || "注册表未记录安装目录（以扫描结果为准）"}
                </div>
                {noUninstaller && (
                  <div className="hint" style={{ marginTop: 6, color: "var(--warn)" }}>
                    未找到卸载程序：将使用强制删除（不运行卸载器）
                  </div>
                )}
              </div>

              {aiGate === "checking" && (
                <div className="row" style={{ marginTop: 10 }}>
                  <span className="spinner" /> 正在检查 AI 配置…
                </div>
              )}

              {aiGate === "missing" && (
                <div className="protected-note" role="alert" style={{ marginTop: 10 }}>
                  <div className="note-head">
                    <span className="tag protected">需要 AI</span>
                    <strong>未配置 AI，无法开始卸载</strong>
                  </div>
                  <p className="reason">
                    卸载后的残留清理依赖 AI 判断应删除哪些项目。请先在设置页配置并启用 AI 后再试。
                  </p>
                  <div className="actions" style={{ marginTop: 12 }}>
                    <button onClick={onClose}>关闭</button>
                    <button className="ghost" onClick={() => void checkAi()}>重新检查</button>
                    <button className="primary" onClick={goSettings}>去设置</button>
                  </div>
                </div>
              )}

              {aiGate === "error" && (
                <div className="protected-note" role="alert" style={{ marginTop: 10 }}>
                  <div className="note-head">
                    <span className="tag protected">AI 检查失败</span>
                    <strong>无法确认 AI 是否可用</strong>
                  </div>
                  <p className="reason">{aiGateError}</p>
                  <div className="actions" style={{ marginTop: 12 }}>
                    <button onClick={onClose}>关闭</button>
                    <button className="primary" onClick={() => void checkAi()}>重试</button>
                  </div>
                </div>
              )}

              {aiGate === "ready" && (
                <>
                  {preRisk ? (
                    <div style={{ marginTop: 10 }}>
                      <div className="row" style={{ marginBottom: 4 }}>
                        <span className="tag">AI 卸载风险</span>
                        <span className={"pill " + riskPill(preRisk.riskLevel)}>
                          {riskLabel(preRisk.riskLevel)}
                        </span>
                        {preRisk.safeToUninstall != null && (
                          <span className="muted" style={{ fontSize: 11 }}>
                            {preRisk.safeToUninstall ? "未发现明显问题" : "发现问题，建议先看说明"}
                          </span>
                        )}
                      </div>
                      <p style={{ margin: "2px 0" }}>{preRisk.recommendation}</p>
                      {(preRisk.issues?.length ?? 0) > 0 && (
                        <div className="hint" style={{ color: "var(--warn)" }}>
                          <ul style={{ margin: "2px 0 0", paddingLeft: 18 }}>
                            {preRisk.issues!.map((s, i) => (
                              <li key={i}>{s}</li>
                            ))}
                          </ul>
                        </div>
                      )}
                      {(preRisk.dependencies.length > 0 ||
                        preRisk.affectedComponents.length > 0) && (
                        <details className="hint" style={{ marginTop: 4 }}>
                          <summary>可能依赖 / 受影响组件</summary>
                          {preRisk.dependencies.length > 0 && (
                            <div>依赖：{preRisk.dependencies.join("、")}</div>
                          )}
                          {preRisk.affectedComponents.length > 0 && (
                            <div>受影响：{preRisk.affectedComponents.join("、")}</div>
                          )}
                        </details>
                      )}
                    </div>
                  ) : (
                    <div className="hint" style={{ marginTop: 10 }}>
                      AI 风险摘要获取中（不影响继续；清理阶段仍会强制 AI 审核）。
                    </div>
                  )}

                  {dangerousRisk && (
                    <label className="checkbox" style={{ marginTop: 8 }}>
                      <input
                        type="checkbox"
                        checked={riskAck}
                        onChange={(e) => setRiskAck(e.target.checked)}
                      />
                      我已了解上述风险，仍要卸载
                    </label>
                  )}

                  {!noUninstaller && (
                    <label className="checkbox" style={{ margin: "8px 0" }}>
                      <input
                        type="checkbox"
                        checked={silent}
                        onChange={(e) => setSilent(e.target.checked)}
                      />
                      优先使用软件提供的静默卸载命令（QuietUninstallString）
                    </label>
                  )}

                  <details className="uninstall-command">
                    <summary>
                      卸载命令：<code>{command.command || "（无可用卸载程序）"}</code>
                    </summary>
                    <pre style={{ marginTop: 6 }}>{command.command || "（注册表没有记录卸载命令）"}</pre>
                    {commandHasSilentFlags && (
                      <div className="hint" style={{ color: "var(--warn)" }}>
                        这条命令本身已含静默参数，执行时可能不弹卸载界面。
                      </div>
                    )}
                    {silent && !command.quietAvailable && (
                      <div className="hint">该软件没有独立的 QuietUninstallString，仍执行上面的命令。</div>
                    )}
                  </details>
                </>
              )}

              <div className="actions">
                <button onClick={onClose}>取消</button>
                <button
                  className="primary"
                  onClick={() => void start()}
                  disabled={!canStart}
                  title={
                    aiGate === "checking"
                      ? "正在检查 AI 配置"
                      : aiGate === "missing"
                        ? "请先在设置页配置并启用 AI"
                        : aiGate === "error"
                          ? "AI 检查失败，请重试"
                          : dangerousRisk && !riskAck
                            ? "请先勾选“我已了解风险”"
                            : ""
                  }
                >
                  {noUninstaller ? "强制删除并清理" : "卸载并分析残留"}
                </button>
              </div>
            </>
          )}

          {step === "working" &&
            (stage === "deleting" || stage === "postcheck" ? (
              <div className="row">
                <span className="spinner" /> {stageLabel(stage)}
              </div>
            ) : (
              <div>
                <div className="row">
                  <span className="spinner" />
                  {stageLabel(stage)}
                </div>
                <ol className="hint" style={{ marginTop: 10, paddingLeft: 20 }}>
                  <li style={{ opacity: stage === "uninstalling" ? 1 : 0.55 }}>
                    运行卸载程序并等待结束
                  </li>
                  <li style={{ opacity: stage === "scanning" ? 1 : 0.55 }}>扫描该软件的残留</li>
                  <li style={{ opacity: stage === "assessing" ? 1 : 0.55 }}>
                    AI 逐项判断应该清理哪些
                  </li>
                </ol>
                {stage === "uninstalling" && (
                  <div className="hint">大型软件可能需要数分钟，最长等待 30 分钟。</div>
                )}
                {log.length > 0 && (
                  <pre style={{ marginTop: 8, maxHeight: 180, overflow: "auto" }}>
                    {log.join("\n")}
                  </pre>
                )}
              </div>
            ))}

          {step === "review" && (
            <>
              {forceMode && (
                <div className="uninstall-command" style={{ marginBottom: 10 }}>
                  <div className="uc-head">
                    <span>强制删除模式</span>
                    <span className="tag">未调用卸载器</span>
                  </div>
                  <div className="hint" style={{ margin: 0 }}>
                    已直接扫描它的文件与注册表。确认后会先尝试创建系统还原点，再删除选中项。
                  </div>
                  {residuals.length > 0 && (
                    <label className="checkbox" style={{ marginTop: 6 }}>
                      <input
                        type="checkbox"
                        checked={removeEntry}
                        onChange={(e) => setRemoveEntry(e.target.checked)}
                      />
                      同时删除注册表里的卸载记录（否则它会一直留在程序列表里）
                    </label>
                  )}
                </div>
              )}

              {forceMode && residuals.length === 0 && (
                <div
                  style={{
                    padding: "12px 14px",
                    border: "1px solid var(--hair)",
                    borderRadius: "var(--radius)",
                    background: "var(--panel-2)",
                  }}
                >
                  <strong>没有找到可删除的残留。</strong>
                  <div className="hint">
                    程序文件可能已经不在了。如果它仍出现在程序列表里，可以只清理注册表里的卸载记录。
                  </div>
                  <label className="checkbox" style={{ marginTop: 8 }}>
                    <input
                      type="checkbox"
                      checked={removeEntry}
                      onChange={(e) => setRemoveEntry(e.target.checked)}
                    />
                    删除注册表里的卸载记录
                  </label>
                  <div className="actions" style={{ marginTop: 10 }}>
                    <button onClick={onClose}>关闭</button>
                    <button
                      className="danger"
                      disabled={!removeEntry}
                      onClick={() => void doClean()}
                    >
                      清理卸载记录
                    </button>
                  </div>
                </div>
              )}

              {!forceMode && !uninstallOk && residuals.length === 0 && (
                <div className="protected-note" role="alert">
                  <div className="note-head">
                    <span className="tag protected">卸载未确认完成</span>
                    <strong>暂不自动扫描</strong>
                  </div>
                  <p className="reason">
                    {uninstallRes?.log?.trim() ||
                      "卸载程序没有正常返回，或者仍在后台删除文件。此时扫描可能读到正在变化的文件。"}
                  </p>
                  <div className="hint">
                    请等待软件自带的卸载窗口结束；必要时重启电脑后再扫描。
                  </div>
                  <div className="actions" style={{ marginTop: 12 }}>
                    <button onClick={onClose}>稍后处理</button>
                    <button
                      className="ghost"
                      onClick={() => void startForce()}
                      title="不运行卸载器，直接扫描并删除它的文件与注册表（删除前尝试创建还原点）"
                    >
                      强制删除
                    </button>
                    <button className="primary" onClick={() => void manualScan()}>
                      仍要扫描残留
                    </button>
                  </div>
                </div>
              )}

              {residuals.length > 0 && (
                <div
                  style={{
                    padding: "10px 12px",
                    border: "1px solid var(--hair)",
                    borderRadius: "var(--radius)",
                    background: "var(--panel-2)",
                  }}
                >
                  <div className="row" style={{ marginBottom: 4 }}>
                    <span className="tag">AI 清理建议</span>
                    {!assessment && !assessError && <span className="spinner" />}
                    {assessment && (
                      <span className={"pill " + overallPill(assessment.overall)}>
                        {overallLabel(assessment.overall)}
                      </span>
                    )}
                  </div>
                  {assessError ? (
                    <div className="hint" style={{ color: "var(--warn)" }}>
                      AI 分析失败：{assessError}
                      <button
                        type="button"
                        className="ghost"
                        style={{ marginLeft: 8 }}
                        onClick={() => void retryAssess()}
                      >
                        重新分析
                      </button>
                      <button
                        type="button"
                        className="ghost"
                        style={{ marginLeft: 6 }}
                        onClick={goSettings}
                        title="去设置页检查 AI 配置；之后可在卸载历史里重新扫描残留"
                      >
                        去设置
                      </button>
                    </div>
                  ) : assessment ? (
                    <div>
                      <p style={{ margin: "2px 0" }}>
                        建议清理 <strong>{picks.length}</strong> 项
                        {picksBytes > 0 ? `（约 ${fmtSize(picksBytes)}）` : ""}
                        {keepCount > 0 ? `；建议保留 ${keepCount} 项` : ""}
                        {cautionCount > 0 ? `；需谨慎 ${cautionCount} 项` : ""}。
                      </p>
                     {aiGuess.guessed > 0 && (
                        <div className="hint">
                          AI 另外推断出 <strong>{aiGuess.guessed}</strong>{" "}
                          处可能被固定规则漏掉的位置
                          {aiGuess.summary ? `：${aiGuess.summary}` : ""}
                          （均已校验确实存在且属于这款软件）
                          <div style={{ marginTop: 4 }}>
                            {residuals
                              .filter((r) => r.id.startsWith("ai:"))
                              .slice(0, 4)
                              .map((r) => (
                                <div key={r.id} className="mono" style={{ fontSize: 11 }}>
                                  {r.path}
                                </div>
                              ))}
                          </div>
                        </div>
                      )}
                      {aiGuess.error && (
                        <div className="hint" style={{ color: "var(--warn)" }}>
                          本次只有固定规则的结果，AI 推断未生效：{aiGuess.error}
                          <button
                            type="button"
                            className="ghost"
                            style={{ marginLeft: 8 }}
                            disabled={suggestRetrying}
                            onClick={() => void retrySuggest()}
                            title="重新扫描并让 AI 推断一次；已勾选的项目会保留"
                          >
                            {suggestRetrying ? "重新推断中…" : "重新推断"}
                          </button>
                        </div>
                      )}
                      <div className="hint">
                        所有候选项都会过一遍安全校验；系统目录、关键服务、Microsoft
                        注册表项由内置规则硬拦截，AI 无法放行。
                      </div>
                    </div>
                  ) : (
                    <div className="hint">AI 正在逐项分析残留…</div>
                  )}
                </div>
              )}

              {residuals.length > 0 && (
                <div className="cleanup-scope" style={{ marginTop: 10 }}>
                  <div className="scope-head">
                    <span className="scope-title">清理范围</span>
                    <span className="scope-total">
                      已选 {picks.length}/{residuals.length} 项 ·{" "}
                      {scanBytes > 0 ? `约 ${fmtSize(scanBytes)}` : "大小未知"}
                    </span>
                    <span className="scope-actions">
                      <button type="button" className="ghost" onClick={selectRecommended}>
                        按 AI 建议
                      </button>
                      <button type="button" className="ghost" onClick={clearAll}>
                        清空
                      </button>
                    </span>
                  </div>
                  {cleanupGroups.map((g) => {
                    const selectable = g.items.filter((it) => verdictOf(it.id) !== "keep");
                    const allSelected =
                      selectable.length > 0 && selectable.every((it) => Boolean(selected[it.id]));
                    return (
                      <div className="scope-row" key={g.key}>
                        <input
                          type="checkbox"
                          className="scope-group-check"
                          checked={allSelected}
                          disabled={selectable.length === 0}
                          onChange={() => toggleGroup(g)}
                          title="选中 / 取消该类的全部项目（AI 建议保留的除外）"
                        />
                        <span className="scope-label">{g.label}</span>
                        <span className="scope-paths" title={g.locations.join("\n")}>
                          {locationsText(g)}
                        </span>
                        <span className="scope-count">{g.items.length} 项</span>
                        <span className="scope-size">{g.bytes > 0 ? fmtSize(g.bytes) : "—"}</span>
                      </div>
                    );
                  })}
                  <div className="scope-foot">
                    {residuals.length > LIST_PREVIEW && (
                      <button type="button" className="ghost" onClick={() => setShowAll((v) => !v)}>
                        {showAll ? "收起清单" : `展开全部 ${residuals.length} 项`}
                      </button>
                    )}
                  </div>
                </div>
              )}

              {residuals.length > 0 && (
                <div style={{ marginTop: 8 }}>
                  {(showAll ? residuals : residuals.slice(0, LIST_PREVIEW)).map((r) => {
                    const v = verdictOf(r.id);
                    const reason = reasonOf(r.id);
                    const kept = v === "keep";
                    return (
                      <div key={r.id} className="res-row">
                        <input
                          type="checkbox"
                          checked={!kept && Boolean(selected[r.id])}
                          disabled={kept}
                          onChange={() => toggleItem(r.id)}
                          title={kept ? "AI 建议保留，不可勾选" : ""}
                        />
                        <div>
                          <div className="path">{r.path}</div>
                          <div className="meta">
                            {r.display}
                            {r.sizeBytes ? ` · ${fmtSize(r.sizeBytes)}` : ""}
                            {r.inUse ? " · 占用中" : ""}
                          </div>
                          {reason && <div className="meta">AI: {reason}</div>}
                        </div>
                        <span className="tag">{kindLabel(r.kind)}</span>
                        {r.id.startsWith("ai:") && <span className="tag">AI 推断</span>}
                        <span className={"pill " + verdictPill(v)}>{v ? verdictLabel(v) : "—"}</span>
                        <button
                          className="ghost"
                          onClick={() => api.openInExplorer(r.path)}
                          title="打开所在位置"
                        >
                          ↗
                        </button>
                      </div>
                    );
                  })}
                </div>
              )}

              {residuals.length === 0 && uninstallOk && (
                <div
                  style={{
                    marginTop: 10,
                    padding: "12px 14px",
                    border: "1px solid #2a4a36",
                    background: "var(--safe-soft)",
                    borderRadius: "var(--radius)",
                  }}
                >
                  <strong>卸载干净：未发现残留。</strong>
                  <div className="hint">
                    已扫描安装目录、应用数据、快捷方式、注册表、服务与计划任务。
                  </div>
                </div>
              )}

              {residuals.length > 0 && (
                <div className="actions" style={{ marginTop: 12 }}>
                  <button onClick={onDone}>保留剩余</button>
                  <button
                    className="danger"
                    disabled={deleteDisabled}
                    title={
                      !assessment
                        ? "AI 分析未完成，暂不能删除"
                        : picks.length === 0
                          ? "没有选中任何项目"
                          : ""
                    }
                    onClick={() => void doClean()}
                  >
                    清理选中的 {picks.length} 项
                  </button>
                </div>
              )}
            </>
          )}

          {step === "done" && (
            <>
              {forceOutcome && (
                <div
                  style={{
                    marginTop: 10,
                    padding: "10px 12px",
                    border: "1px solid var(--hair)",
                    borderRadius: "var(--radius)",
                    background: "var(--panel-2)",
                  }}
                >
                  <div className="row">
                    <span className="tag">强制删除</span>
                    <span className={"pill " + (forceOutcome.restorePoint ? "safe" : "warn")}>
                      {forceOutcome.restorePoint ? "已建还原点" : "无还原点"}
                    </span>
                  </div>
                  <div className="hint" style={{ marginTop: 4 }}>
                    {forceOutcome.restorePoint ||
                      forceOutcome.restorePointError ||
                      "还原点状态未知"}
                  </div>
                  {forceOutcome.entryRemoved && (
                    <div className="hint">已删除注册表里的卸载记录。</div>
                  )}
                  {forceOutcome.entryError && (
                    <div className="hint" style={{ color: "var(--warn)" }}>
                      卸载记录未删除：{forceOutcome.entryError}
                    </div>
                  )}
                </div>
              )}

              {bytesFreed > 0 && (
                <div className="hero">
                  <div className="big">{fmtSize(bytesFreed)}</div>
                  <div className="label">已释放空间</div>
                </div>
              )}
              {deleteResults.length > 0 && (
                <details style={{ marginTop: 10 }}>
                  <summary className="hint" style={{ cursor: "pointer" }}>
                    清理明细：共 {deleteResults.length} 项，成功 {successCount} 项
                  </summary>
                  <div style={{ marginTop: 4 }}>
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
                          <div
                            className="mono"
                            style={{ fontSize: 11, wordBreak: "break-all" }}
                          >
                            {r.id}
                          </div>
                          <div className="muted" style={{ fontSize: 10, marginTop: 2 }}>
                            {r.message}
                          </div>
                        </div>
                      </div>
                    ))}
                  </div>
                </details>
              )}

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
                  <span className="tag">是否卸载干净</span>
                  {(postChecking || cleanChecking) && <span className="spinner" />}
                  {!postChecking && !cleanChecking && cleanVerdict && (
                    <span className={"pill " + (cleanVerdict.clean ? "safe" : "warn")}>
                      {cleanVerdict.clean ? "已卸载干净" : `仍有 ${leftoverItems.length} 项残留`}
                    </span>
                  )}
                  {!postChecking && !cleanChecking && !cleanVerdict && (
                    <span className="pill warn">复查未完成</span>
                  )}
                </div>

                <div className="hint" style={{ marginTop: 4 }}>
                  {postChecking
                    ? "正在重新扫描残留…"
                    : cleanChecking
                      ? "AI 正在判断这些剩余是否影响卸载完成度…"
                      : cleanVerdict
                        ? cleanVerdict.summary
                        : postCheck === null
                          ? "复查未完成，可关闭后重新打开向导扫描。"
                          : "AI 结论获取失败，可点击下方按钮重新判断或直接清理剩余项。"}
                </div>
                {cleanVerdict?.nextStep && <div className="hint">{cleanVerdict.nextStep}</div>}
                {cleanVerdict?.byAi && leftoverItems.length > 0 && (
                  <div className="muted" style={{ fontSize: 10, marginTop: 2 }}>
                    结论基于本次扫描到的 {leftoverItems.length} 项剩余；扫描范围之外的文件 AI
                    看不到，需你自行确认。
                  </div>
                )}

                {!postChecking && !cleanChecking && leftoverItems.length > 0 && (
                  <details style={{ marginTop: 8 }} open={cleanVerdict?.clean === false}>
                    <summary className="hint" style={{ cursor: "pointer" }}>
                      查看剩余 {leftoverItems.length} 项及 AI 建议
                    </summary>
                    <div style={{ marginTop: 4 }}>
                      {leftoverItems.map((r) => {
                        const v = leftoverVerdictOf(r.id);
                        return (
                          <div
                            key={r.id}
                            style={{
                              display: "flex",
                              gap: 8,
                              alignItems: "flex-start",
                              padding: "6px 0",
                              borderBottom: "1px solid var(--hair)",
                            }}
                          >
                            {v && (
                              <span className={"pill " + verdictPill(v.verdict)}>
                                {verdictLabel(v.verdict)}
                              </span>
                            )}
                            <div style={{ flex: 1, minWidth: 0 }}>
                              <div
                                className="mono"
                                style={{ fontSize: 11, wordBreak: "break-all" }}
                              >
                                {r.path}
                              </div>
                              <div className="muted" style={{ fontSize: 10, marginTop: 2 }}>
                                {v?.reason || r.detail || kindLabel(r.kind)}
                              </div>
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  </details>
                )}

                {!postChecking && !cleanChecking && leftoverItems.length > 0 && (
                  <div className="row" style={{ marginTop: 8 }}>
                    <button onClick={() => void cleanAgain()}>再次清理剩余</button>
                  </div>
                )}
                {cleanReportError && !postChecking && !cleanChecking && (
                  <div className="hint" style={{ color: "var(--warn)", marginTop: 4 }}>
                    AI 判断失败：{cleanReportError}
                  </div>
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

function stageLabel(stage: WorkStage) {
  switch (stage) {
    case "uninstalling":
      return "正在运行卸载程序并等待结束";
    case "scanning":
      return "正在扫描残留，并让 AI 推断可能遗漏的位置";
    case "assessing":
      return "AI 正在分析应该清理哪些项目";
    case "deleting":
      return "正在删除选中的残留";
    case "postcheck":
      return "正在复查残留，并让 AI 判断是否卸载干净";
  }
}

/** 三步进度条：done 归到「清理与复查」。 */
function Steps({ current }: { current: Step }) {
  const labels = ["确认", "卸载与分析", "清理与复查"];
  const active = current === "intro" ? 0 : current === "working" ? 1 : 2;
  return (
    <div className="steps">
      {labels.map((label, i) => (
        <span key={label} className={"step" + (i < active ? " done" : i === active ? " now" : "")}>
          {label}
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
