/**
 * Dev-only mock of @tauri-apps/api/core for previewing the UI in a plain browser.
 * Not bundled into the Tauri app (vite alias used only by tools/preview/vite.config.mock.ts).
 */

const q = new URLSearchParams(location.search);
const state = q.get("state") ?? "static"; // static | env | fail
const persist = q.get("persist") === "1";
const STORE_KEY = "preview-ai-config";

interface MockCfg {
  enabled: boolean;
  baseUrl: string;
  apiKey: string;
  model: string;
  useEnv: boolean;
  envApiKeyName: string;
  envBaseUrlName: string;
  envModelName: string;
  temperature: number;
  timeoutSecs: number;
}

const DEFAULTS: MockCfg = {
  enabled: true,
  baseUrl: "https://api.deepseek.com/v1",
  apiKey: "sk-preview-0123456789abcdef9f2c",
  model: "deepseek-flash",
  useEnv: false,
  envApiKeyName: "DEEPSEEK_API_KEY",
  envBaseUrlName: "DEEPSEEK_BASE_URL",
  envModelName: "DEEPSEEK_MODEL",
  temperature: 0.2,
  timeoutSecs: 90,
};

const ENV_DEFAULTS: MockCfg = {
  ...DEFAULTS,
  useEnv: true,
  envApiKeyName: "DEEPSEEK_API_KEY",
  envBaseUrlName: "DEEPSEEK_BASE_URL",
  envModelName: "DEEPSEEK_MODEL",
};

// 模拟用户当前的真实配置文件：已启用、DeepSeek、但没有填 API Key。
const NOKEY_DEFAULTS: MockCfg = {
  ...DEFAULTS,
  apiKey: "",
};

const CONFIG_PATH = "C:\\Users\\preview\\AppData\\Roaming\\CleanSweep\\ai_config.json";

function loadCfg(): MockCfg {
  const base = state === "env" ? ENV_DEFAULTS : state === "nokey" ? NOKEY_DEFAULTS : DEFAULTS;
  if (persist) {
    try {
      const raw = localStorage.getItem(STORE_KEY);
      if (raw) return { ...base, ...(JSON.parse(raw) as Partial<MockCfg>) };
    } catch {
      /* ignore */
    }
  }
  return { ...base };
}

function saveCfg(cfg: MockCfg) {
  if (persist) localStorage.setItem(STORE_KEY, JSON.stringify(cfg));
}

function maskKey(k: string): string {
  if (!k) return "";
  const chars = [...k];
  if (chars.length <= 8) return "••••";
  return `${chars.slice(0, 4).join("")}…${chars.slice(-4).join("")}`;
}

function effective(cfg: MockCfg) {
  const envOn = cfg.useEnv;
  return {
    enabled: cfg.enabled,
    ready: cfg.enabled && cfg.baseUrl.trim() !== "" && cfg.model.trim() !== "",
    baseUrl: envOn ? "https://api.deepseek.com/v1" : cfg.baseUrl,
    baseUrlFromEnv: envOn,
    model: envOn ? "deepseek-flash" : cfg.model,
    modelFromEnv: envOn,
    apiKeyMasked: maskKey(cfg.apiKey),
    apiKeyFromEnv: envOn,
    useEnv: cfg.useEnv,
    configPath: CONFIG_PATH,
  };
}

function delay(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}

const MODELS: Record<string, string[]> = {
  openai: ["gpt-6-luna", "gpt-6", "gpt-4o-mini"],
  deepseek: ["deepseek-flash", "deepseek-v4-pro"],
  openrouter: ["openai/gpt-5-nano", "openai/gpt-4o-mini"],
  qwen: ["qwen-plus", "qwen-turbo", "qwen-max"],
  ollama: ["qwen3:8b", "gpt-oss:20b", "deepseek-r1:7b"],
  lmstudio: ["local-model"],
};

let wizardDeleted = false;

export const SAMPLE_APPS = [
  {
    id: "RegistryMachine64::Google Chrome",
    displayName: "Google Chrome",
    publisher: "Google LLC",
    version: "141.0.7390.55",
    installLocation: "C:\\Program Files\\Google\\Chrome\\Application",
    installDate: "2026-03-01",
    uninstallString: '"C:\\Program Files\\Google\\Chrome\\Application\\setup.exe" --uninstall',
    quietUninstallString: null,
    displayIcon: null,
    estimatedSizeKb: 512000,
    source: "registrymachine64",
    registryKey:
      "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Google Chrome",
    isSystemComponent: false,
    isProtected: false,
    protectedReason: null,
    packageFamily: null,
  },
  {
    id: "RegistryMachine64::7-Zip",
    displayName: "7-Zip 24.09",
    publisher: "Igor Pavlov",
    version: "24.09",
    installLocation: "C:\\Program Files\\7-Zip",
    installDate: "2026-02-11",
    uninstallString: '"C:\\Program Files\\7-Zip\\Uninstall.exe"',
    quietUninstallString: null,
    displayIcon: null,
    estimatedSizeKb: 5600,
    source: "registrymachine64",
    registryKey: "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\7-Zip",
    isSystemComponent: false,
    isProtected: false,
    protectedReason: null,
    packageFamily: null,
  },
  {
    id: "RegistryMachine64::Microsoft Edge",
    displayName: "Microsoft Edge",
    publisher: "Microsoft Corporation",
    version: "141.0.3537.57",
    installLocation: "C:\\Program Files (x86)\\Microsoft\\Edge\\Application",
    installDate: "2026-09-20",
    uninstallString: "",
    quietUninstallString: null,
    displayIcon: null,
    estimatedSizeKb: 420000,
    source: "registrymachine64",
    registryKey: "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Microsoft Edge",
    isSystemComponent: true,
    isProtected: true,
    protectedReason: "Windows 系统组件/内置浏览器，卸载可能影响系统功能",
    packageFamily: null,
  },
  {
    id: "RegistryMachine64::KB5031234",
    displayName: "Security Update for Windows (KB5031234)",
    publisher: "Microsoft Corporation",
    version: "1",
    installLocation: null,
    installDate: "2026-08-10",
    uninstallString: "",
    quietUninstallString: null,
    displayIcon: null,
    estimatedSizeKb: null,
    source: "registrymachine64",
    registryKey:
      "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\KB5031234",
    isSystemComponent: true,
    isProtected: true,
    protectedReason: "Windows 更新/系统补丁",
    packageFamily: null,
  },
  {
    id: "RegistryUser::Microsoft VS Code",
    displayName: "Microsoft Visual Studio Code",
    publisher: "Microsoft Corporation",
    version: "1.97.0",
    installLocation: "C:\\Users\\preview\\AppData\\Local\\Programs\\Microsoft VS Code",
    installDate: "2026-01-05",
    uninstallString:
      '"C:\\Users\\preview\\AppData\\Local\\Programs\\Microsoft VS Code\\Uninstall.exe" /S',
    quietUninstallString: null,
    displayIcon: null,
    estimatedSizeKb: 380000,
    source: "registryuser",
    registryKey: "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\VS Code",
    isSystemComponent: false,
    isProtected: false,
    protectedReason: null,
    packageFamily: null,
  },
  {
    id: "store::SpotifyAB.SpotifyMusic_1.260.0.0_x64__zpdnekdrzrea0",
    displayName: "Spotify Music",
    publisher: "Spotify AB",
    version: "1.260.0.0",
    installLocation: "C:\\Program Files\\WindowsApps\\SpotifyAB.SpotifyMusic_1.260.0.0_x64__zpdnekdrzrea0",
    installDate: null,
    uninstallString:
      "Remove-AppxPackage -Package 'SpotifyAB.SpotifyMusic_1.260.0.0_x64__zpdnekdrzrea0'",
    quietUninstallString:
      "Remove-AppxPackage -Package 'SpotifyAB.SpotifyMusic_1.260.0.0_x64__zpdnekdrzrea0'",
    displayIcon: null,
    estimatedSizeKb: null,
    source: "store",
    registryKey: null,
    isSystemComponent: false,
    isProtected: false,
    protectedReason: null,
    packageFamily: "SpotifyAB.SpotifyMusic_zpdnekdrzrea0",
  },
];

const SAMPLE_RULE = {
  name: "CleanSweep_Block_Telegram",
  exePath: "C:\\Users\\preview\\AppData\\Roaming\\Telegram Desktop\\Telegram.exe",
  directionIn: true,
  directionOut: true,
  enabled: true,
};

const SAMPLE_AUTOSTART = [
  {
    id: "reg:HKCU|Telegram",
    kind: "registry",
    name: "Telegram",
    value: "C:\\Users\\preview\\AppData\\Roaming\\Telegram Desktop\\Telegram.exe --autostart",
    location: "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run",
    disabled: false,
  },
  {
    id: "task:\\MyApp\\Updater",
    kind: "task",
    name: "\\MyApp\\Updater",
    value: null,
    location: "ScheduledTask",
    disabled: false,
  },
  {
    id: "reg:HKCU|disabled_Spotify",
    kind: "registry",
    name: "disabled_Spotify",
    value: "C:\\Users\\preview\\AppData\\Roaming\\Spotify\\Spotify.exe",
    location: "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run",
    disabled: true,
  },
];

const SAMPLE_CAPS = [
  { capability: "location", label: "位置", packageFamily: "SpotifyAB.SpotifyMusic_zpdnekdrzrea0", value: "Allow" },
  { capability: "webcam", label: "摄像头", packageFamily: "SpotifyAB.SpotifyMusic_zpdnekdrzrea0", value: null },
  { capability: "microphone", label: "麦克风", packageFamily: "SpotifyAB.SpotifyMusic_zpdnekdrzrea0", value: "Deny" },
  { capability: "userAccountInformation", label: "账户信息", packageFamily: "SpotifyAB.SpotifyMusic_zpdnekdrzrea0", value: "Prompt" },
];


const WIZARD_RESIDUALS = [
  {
    id: "install_dir:C:\\Program Files\\DemoApp",
    kind: "directory",
    path: "C:\\Program Files\\DemoApp",
    display: "安装目录: C:\\Program Files\\DemoApp",
    detail: "12.3 MB",
    sizeBytes: 12_900_000,
    inUse: false,
  },
  {
    id: "reg:HKCU:SOFTWARE\\DemoApp",
    kind: "registrykey",
    path: "HKCU\\SOFTWARE\\DemoApp",
    display: "HKCU\\SOFTWARE\\DemoApp",
    detail: "2 个子键",
    sizeBytes: null,
    inUse: false,
  },
  {
    id: "shortcut:C:\\Users\\preview\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\DemoApp.lnk",
    kind: "shortcut",
    path: "C:\\Users\\preview\\AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\DemoApp.lnk",
    display: "DemoApp.lnk",
    detail: null,
    sizeBytes: 1200,
    inUse: false,
  },
];

const WIZARD_APP = {
  id: "RegistryUser::DemoApp",
  displayName: "DemoApp",
  publisher: "Demo Vendor",
  version: "2.3.1",
  installLocation: "C:\\Program Files\\DemoApp",
  installDate: "2026-01-02",
  uninstallString: "C:\\Program Files\\DemoApp\\uninstall.exe /S",
  quietUninstallString: "C:\\Program Files\\DemoApp\\uninstall.exe /S",
  displayIcon: null,
  estimatedSizeKb: 12_600,
  source: "registryuser",
  registryKey:
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\DemoApp",
  isSystemComponent: false,
  isProtected: false,
  protectedReason: null,
};

export function previewWizardApp() {
  return WIZARD_APP;
}


function modelsFor(cfg: MockCfg): string[] {
  const b = (cfg.baseUrl || "").toLowerCase();
  const m = (cfg.model || "").toLowerCase();
  if (b.includes("deepseek") || m.startsWith("deepseek")) return MODELS.deepseek;
  if (b.includes("openrouter")) return MODELS.openrouter;
  if (b.includes("dashscope") || m.startsWith("qwen")) return MODELS.qwen;
  if (b.includes("11434") || b.includes("ollama")) return MODELS.ollama;
  if (b.includes("1234")) return MODELS.lmstudio;
  return MODELS.openai;
}

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const cfg = (args?.cfg as MockCfg) ?? loadCfg();
  switch (cmd) {
    case "get_ai_config":
      return loadCfg() as unknown as T;

    case "save_ai_config":
      saveCfg(cfg);
      return undefined as T;

    case "resolve_ai_effective":
      return effective(loadCfg()) as unknown as T;

    case "probe_env_vars": {
      const names = (args?.names as string[] | undefined) ?? [];
      const out: Record<string, { found: boolean; masked: string }> = {};
      for (const name of names) {
        if (!name) continue;
        if (state === "env") {
          const set = /API_KEY/.test(name) || /BASE_URL/.test(name) || /MODEL/.test(name);
          out[name] = set
            ? { found: true, masked: /API_KEY/.test(name) ? "sk-p…9f2c" : name.includes("BASE_URL") ? "https://api.deepseek.com/v1" : "deepseek-flash" }
            : { found: false, masked: "" };
        } else {
          out[name] = { found: false, masked: "" };
        }
      }
      return out as unknown as T;
    }

    case "test_ai_connection": {
      await delay(900);
      if (state === "fail") {
        throw "HTTP 401 Unauthorized: Invalid API key provided. 请检查 API Key 是否正确，或该服务是否需要 Key。";
      }
      return {
        model: cfg.model || "deepseek-flash",
        latencyMs: 823,
        message: "连接正常，回复：OK",
      } as unknown as T;
    }

    case "list_ai_models": {
      await delay(700);
      if (state === "fail") throw "获取模型列表失败: HTTP 404 Not Found（该服务商可能不支持 /models）";
      return modelsFor(cfg) as unknown as T;
    }

    // ===== 卸载向导 mock =====
    case "ai_analyze_risk": {
      await delay(700);
      if (state === "fail") throw "AI 未启用。请在设置中启用 AI 后再试。";
      const name = (args?.app as { displayName?: string } | undefined)?.displayName ?? "该软件";
      return {
        riskLevel: "medium",
        dependencies: ["其他程序可能共享的运行库"],
        affectedComponents: [`${name} 的自动更新服务`, "正在运行的实例"],
        recommendation: `${name} 可以卸载。卸载前建议关闭正在运行的程序，并确认没有其他软件依赖它的自动更新服务。`,
      } as unknown as T;
    }

    case "uninstall_app": {
      await delay(600);
      return {
        success: true,
        exitCode: 0,
        log: "退出码 0",
        requiresReboot: false,
      } as unknown as T;
    }

    case "scan_residuals_for": {
      await delay(700);
      return (wizardDeleted ? [] : WIZARD_RESIDUALS) as unknown as T;
    }

    case "ai_assess_residuals": {
      await delay(900);
      if (state === "fail") throw "AI 未启用。请在设置中启用 AI 后再试。";
      return {
        overall: "caution",
        perItem: WIZARD_RESIDUALS.map((r, i) => ({
          residualId: r.id,
          verdict: i === 1 ? "caution" : "safe",
          reason:
            i === 1
              ? "注册表项可能仍被其他组件引用，建议谨慎删除。"
              : "应用专属残留，可以安全删除。",
        })),
      } as unknown as T;
    }

    case "delete_residuals": {
      await delay(500);
      wizardDeleted = true;
      const items = (args?.items as Array<{ id: string }> | undefined) ?? [];
      return items.map((it) => ({
        id: it.id,
        success: true,
        message: "已删除",
      })) as unknown as T;
    }

    // ===== 软件列表 / 权限页 mock =====
    case "list_installed_apps":
      await delay(300);
      return SAMPLE_APPS as unknown as T;

    case "ai_analyze_app": {
      const app = (args?.app as { displayName?: string } | undefined)?.displayName ?? "该软件";
      await delay(600);
      return {
        summary: `${app} 是一款常用的桌面应用，用于日常办公与内容浏览。`,
        category: "工具/浏览器",
        isSafeToRemove: true,
        riskNotes: ["卸载前请保存未完成的工作", "自定义配置可能不会随卸载删除"],
        alternatives: ["系统自带应用", "同类开源替代品"],
      } as unknown as T;
    }

    case "guess_main_exe": {
      const app = args?.app as { installLocation?: string | null } | undefined;
      const loc = app?.installLocation;
      return (loc ? [`${loc}\\app.exe`] : []) as unknown as T;
    }

    case "firewall_list_managed":
      return [SAMPLE_RULE] as unknown as T;
    case "firewall_is_blocked":
      return true as unknown as T;
    case "firewall_block":
      return "已禁止该程序联网" as unknown as T;
    case "firewall_unblock":
      return "已恢复该程序联网" as unknown as T;

    case "list_capabilities":
      return [
        ["location", "位置"],
        ["webcam", "摄像头"],
        ["microphone", "麦克风"],
        ["userAccountInformation", "账户信息"],
        ["contacts", "联系人"],
      ] as unknown as T;

    case "list_package_capabilities":
      return SAMPLE_CAPS as unknown as T;

    case "list_autostart":
      return SAMPLE_AUTOSTART as unknown as T;

    case "disable_autostart":
    case "enable_autostart":
    case "remove_autostart":
      return "操作已执行" as unknown as T;

    case "set_capability":
      return "已更新能力设置" as unknown as T;

    default:
      return [] as unknown as T;
  }
}

export class Channel<T> {
  onmessage?: (message: T) => void;
}

export const isTauri = false;
