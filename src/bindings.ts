import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface InstalledApp {
  id: string;
  displayName: string;
  publisher?: string | null;
  version?: string | null;
  installLocation?: string | null;
  installDate?: string | null;
  uninstallString?: string | null;
  quietUninstallString?: string | null;
  displayIcon?: string | null;
  estimatedSizeKb?: number | null;
  source: "registrymachine64" | "registrymachine32" | "registryuser" | "store" | "steam";
  registryKey?: string | null;
  isSystemComponent: boolean;
  /** 受保护条目（系统组件 / Windows 更新 / 驱动 / 运行库 / 不可移除的 Store 包）。
   *  后端重校验同时拒绝这类条目；旧版后端可能不返回该字段，前端一律按 false 处理。 */
  isProtected?: boolean;
  /** 受保护原因（简短中文说明），用于列表悬停提示与详情页警告。 */
  protectedReason?: string | null;
  /** Store 应用的 PackageFamilyName（能力管理用）；旧后端可能不返回。 */
  packageFamily?: string | null;
}

export type ResidualKind =
  | "file" | "directory" | "registrykey" | "registryvalue" | "shortcut"
  | "service" | "scheduledtask" | "envvar";

export type AiSafetyVerdict = "safe" | "caution" | "keep" | "unknown";

export interface ResidualItem {
  id: string;
  kind: ResidualKind;
  path: string;
  display: string;
  detail?: string | null;
  sizeBytes?: number | null;
  inUse: boolean;
  aiVerdict?: AiSafetyVerdict;
}

export interface AiAppInfo {
  summary: string;
  category?: string | null;
  isSafeToRemove: boolean;
  riskNotes: string[];
  alternatives: string[];
}

export interface AiUninstallRisk {
  riskLevel: "low" | "medium" | "high" | "critical";
  safeToUninstall?: boolean | null;
  issues?: string[];
  targets?: string[];
  dependencies: string[];
  affectedComponents: string[];
  recommendation: string;
}

export interface AiResidualItemVerdict {
  residualId: string;
  verdict: AiSafetyVerdict;
  reason: string;
}

export interface AiResidualAssessment {
  overall: AiSafetyVerdict;
  perItem: AiResidualItemVerdict[];
}

/** 扫描 + AI 推断的合并结果。 */
export interface SuggestedResiduals {
  items: ResidualItem[];
  /** AI 对推断结果的一句话说明（AI 不可用时为空）。 */
  aiSummary: string;
  /** 其中由 AI 推断并通过校验的项数。 */
  aiGuessed: number;
  /** AI 推断失败的原因；此时 items 只有规则扫描结果。 */
  aiError?: string | null;
}

/** 卸载后复查结论：这款软件算不算卸载干净。 */
export interface AiCleanliness {
  /** 模型未给出时按 verdict 兜底。 */
  clean?: boolean | null;
  verdict?: AiSafetyVerdict | null;
  summary: string;
  nextStep: string;
  perItem: AiResidualItemVerdict[];
}

export interface UninstallResult {
  success: boolean;
  exitCode?: number | null;
  log: string;
  requiresReboot: boolean;
}

export interface DeleteResult {
  id: string;
  success: boolean;
  message: string;
}

export interface ForceRemoveOutcome {
  restorePoint: string | null;
  restorePointError: string | null;
  results: DeleteResult[];
  entryRemoved: boolean;
  entryError: string | null;
}

export interface AiConfig {
  enabled: boolean;
  baseUrl: string;
  apiKey: string;
  model: string;
  useEnv: boolean;
  envApiKeyName: string;
  envBaseUrlName: string;
  envModelName: string;
  /** 0..2，默认 0.2 */
  temperature: number;
  /** 5..600 秒，默认 90 */
  timeoutSecs: number;
}

export interface EnvProbe {
  found: boolean;
  /** 已设置时返回打码后的值（例如 sk-…abcd）；未设置时为空串 */
  masked: string;
}

export interface EffectiveAi {
  enabled: boolean;
  /** enabled && baseUrl && model（Key 可留空，本地服务不需要） */
  ready: boolean;
  baseUrl: string;
  baseUrlFromEnv: boolean;
  model: string;
  modelFromEnv: boolean;
  apiKeyMasked: string;
  apiKeyFromEnv: boolean;
  useEnv: boolean;
  configPath: string;
}

export interface AiTestResult {
  model: string;
  latencyMs: number;
  message: string;
}

export interface FirewallRule {
  name: string;
  exePath: string;
  directionIn: boolean;
  directionOut: boolean;
  enabled: boolean;
}

export interface CapPermission {
  capability: string;
  label: string;
  packageFamily: string | null;
  value: string | null;
}

export interface AutostartEntry {
  id: string;
  kind: string;
  name: string;
  value: string | null;
  location: string;
  disabled: boolean;
}

export interface UninstallRecord {
  id: number;
  app: InstalledApp;
  /** Unix 毫秒时间戳 */
  uninstalledAt: number;
  success: boolean;
  exitCode: number | null;
  log: string;
  residualCount: number;
  lastScanAt: number | null;
  residuals: ResidualItem[];
  aiSummary: string | null;
}

export const api = {
  listInstalledApps: () => invoke<InstalledApp[]>("list_installed_apps"),
  getAiConfig: () => invoke<AiConfig>("get_ai_config"),
  /** 当前进程是否管理员权限（dev/debug 版不会自动提权）。 */
  isElevated: () => invoke<boolean>("is_elevated"),
  saveAiConfig: (cfg: AiConfig) => invoke<void>("save_ai_config", { cfg }),
  probeEnvVars: (names: string[]) =>
    invoke<Record<string, EnvProbe>>("probe_env_vars", { names }),
  resolveAiEffective: () => invoke<EffectiveAi>("resolve_ai_effective"),
  // 用传入的 cfg 直接测试连接，不保存、不启用 AI；失败时 reject（字符串）
  testAiConnection: (cfg: AiConfig) =>
    invoke<AiTestResult>("test_ai_connection", { cfg }),
  listAiModels: (cfg: AiConfig) => invoke<string[]>("list_ai_models", { cfg }),
  aiAnalyzeApp: (app: InstalledApp) => invoke<AiAppInfo>("ai_analyze_app", { app }),
  aiAnalyzeRisk: (app: InstalledApp) => invoke<AiUninstallRisk>("ai_analyze_risk", { app }),
  uninstallApp: (app: InstalledApp, silent: boolean) =>
    invoke<UninstallResult>("uninstall_app", { app, silent }),
  scanResidualsFor: (app: InstalledApp) => invoke<ResidualItem[]>("scan_residuals_for", { app }),
  listUninstallHistory: (limit?: number) =>
    invoke<UninstallRecord[]>("list_uninstall_history", { limit: limit ?? null }),
    scanHistoryResiduals: (id: number) =>
      invoke<ResidualItem[]>("scan_history_residuals", { id }),
    /**
     * 历史页的重扫：固定规则 + AI 推断补漏。
     * 应用已经卸载，名字和残留目录名对不上时（网易云音乐 / Netease），
     * 只跑固定规则会得到“0 项”的假干净结论。
     */
    aiSuggestHistoryResiduals: (id: number) =>
      invoke<SuggestedResiduals>("ai_suggest_history_residuals", { id }),
  deleteUninstallHistory: (id: number) =>
    invoke<void>("delete_uninstall_history", { id }),
  aiAssessResiduals: (app: InstalledApp, residuals: ResidualItem[]) =>
    invoke<AiResidualAssessment>("ai_assess_residuals", { app, residuals }),
  /** 扫描残留，并让 AI 推断规则可能漏掉的位置（推断结果已逐条校验）。 */
  aiSuggestResiduals: (app: InstalledApp) =>
    invoke<SuggestedResiduals>("ai_suggest_residuals", { app }),
  /** 删除后复查：AI 判断这款软件算不算卸载干净。 */
  aiVerifyClean: (app: InstalledApp, cleaned: number, leftovers: ResidualItem[]) =>
    invoke<AiCleanliness>("ai_verify_clean", { app, cleaned, leftovers }),
  deleteResiduals: (items: ResidualItem[]) => invoke<DeleteResult[]>("delete_residuals", { items }),
  /**
   * 强制删除：不运行自带卸载器，直接删掉选中项，并（可选）清理注册表卸载记录。
   * 删除前会尝试创建系统还原点，失败原因在返回值里如实带回。
   */
  forceRemoveApp: (app: InstalledApp, items: ResidualItem[], removeEntry: boolean) =>
    invoke<ForceRemoveOutcome>("force_remove_app", { app, items, removeEntry }),
  getCachedResiduals: (appId: string) => invoke<ResidualItem[]>("get_cached_residuals", { appId }),
  openInExplorer: (path: string) => invoke<void>("open_in_explorer", { path }),
  // 权限管理
  firewallIsBlocked: (exe: string) => invoke<boolean>("firewall_is_blocked", { exe }),
  firewallBlock: (exe: string) => invoke<string>("firewall_block", { exe }),
  firewallUnblock: (exe: string) => invoke<string>("firewall_unblock", { exe }),
  firewallListManaged: () => invoke<FirewallRule[]>("firewall_list_managed"),
  listCapabilities: () => invoke<[string, string][]>("list_capabilities"),
  listPackageCapabilities: (packageFamily: string) =>
    invoke<CapPermission[]>("list_package_capabilities", { packageFamily }),
  setCapability: (cap: string, packageFamily: string, value: string) =>
    invoke<string>("set_capability", { cap, packageFamily, value }),
  guessMainExe: (app: InstalledApp) => invoke<string[]>("guess_main_exe", { app }),
  listAutostart: (appName?: string | null) =>
    invoke<AutostartEntry[]>("list_autostart", { appName: appName ?? null }),
  disableAutostart: (entry: AutostartEntry) => invoke<string>("disable_autostart", { entry }),
  enableAutostart: (entry: AutostartEntry) => invoke<string>("enable_autostart", { entry }),
  removeAutostart: (entry: AutostartEntry) => invoke<string>("remove_autostart", { entry }),
};

export function onEvent<T>(name: string, cb: (p: T) => void): Promise<UnlistenFn> {
  return listen<T>(name, (e) => cb(e.payload));
}
