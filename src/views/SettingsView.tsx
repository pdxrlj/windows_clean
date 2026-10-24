import { useContext, useEffect, useRef, useState } from "react";
import {
  api,
  type AiConfig,
  type AiTestResult,
  type EffectiveAi,
  type EnvProbe,
} from "../bindings";
import { ToastCtx } from "../components/Toast";

/* ------------------------------------------------------------------ */
/* 服务商预设（与后端约定完全一致，修改前先核对契约）                    */
/* ------------------------------------------------------------------ */

interface Preset {
  key: string;
  label: string;
  base: string;
  model: string;
  envKey: string;
  envBase: string;
  envModel: string;
  custom?: boolean;
}

const PRESETS: Preset[] = [
  {
    key: "openai",
    label: "OpenAI",
    base: "https://api.openai.com/v1",
    model: "gpt-6-luna",
    envKey: "OPENAI_API_KEY",
    envBase: "OPENAI_BASE_URL",
    envModel: "OPENAI_MODEL",
  },
  {
    key: "deepseek",
    label: "DeepSeek",
    base: "https://api.deepseek.com/v1",
    model: "deepseek-flash",
    envKey: "DEEPSEEK_API_KEY",
    envBase: "DEEPSEEK_BASE_URL",
    envModel: "DEEPSEEK_MODEL",
  },
  {
    key: "openrouter",
    label: "OpenRouter",
    base: "https://openrouter.ai/api/v1",
    model: "openai/gpt-5-nano",
    envKey: "OPENROUTER_API_KEY",
    envBase: "OPENROUTER_BASE_URL",
    envModel: "OPENROUTER_MODEL",
  },
  {
    key: "qwen",
    label: "通义千问",
    base: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    model: "qwen-plus",
    envKey: "DASHSCOPE_API_KEY",
    envBase: "DASHSCOPE_BASE_URL",
    envModel: "DASHSCOPE_MODEL",
  },
  {
    key: "ollama",
    label: "Ollama（本地）",
    base: "http://127.0.0.1:11434/v1",
    model: "qwen3:8b",
    envKey: "",
    envBase: "",
    envModel: "",
  },
  {
    key: "lmstudio",
    label: "LM Studio（本地）",
    base: "http://127.0.0.1:1234/v1",
    model: "",
    envKey: "",
    envBase: "",
    envModel: "",
  },
];

const CUSTOM_PRESET: Preset = {
  key: "custom",
  label: "自定义",
  base: "",
  model: "",
  envKey: "",
  envBase: "",
  envModel: "",
  custom: true,
};

const PRESET_CARDS = [...PRESETS, CUSTOM_PRESET];

/* ------------------------------------------------------------------ */
/* 小工具                                                              */
/* ------------------------------------------------------------------ */

function errText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}

function normalizeBase(s: string): string {
  return s.trim().replace(/\/+$/, "").toLowerCase();
}

function truncMiddle(s: string, max = 36): string {
  if (s.length <= max) return s;
  const head = Math.ceil((max - 1) / 2);
  const tail = Math.floor((max - 1) / 2);
  return s.slice(0, head) + "…" + s.slice(s.length - tail);
}

function uniqueNames(names: string[]): string[] {
  return [...new Set(names.map((n) => n.trim()).filter(Boolean))];
}

/** 粗判 Base URL 是否已经像个地址，避免用户刚输入 “http” 时就去请求模型列表。 */
function looksLikeEndpoint(s: string): boolean {
  const t = s.trim();
  if (t.length < 4) return false;
  return t.includes("://") || t.includes(".") || /:\d+/.test(t);
}

/** 防御旧后端返回值里缺少新字段的情况（契约是权威，这里只做兜底）。 */
function normalizeConfig(c: AiConfig): AiConfig {
  return {
    enabled: Boolean(c.enabled),
    baseUrl: c.baseUrl ?? "",
    apiKey: c.apiKey ?? "",
    model: c.model ?? "",
    useEnv: Boolean(c.useEnv),
    envApiKeyName: c.envApiKeyName ?? "",
    envBaseUrlName: c.envBaseUrlName ?? "",
    envModelName: c.envModelName ?? "",
    temperature:
      typeof c.temperature === "number" && Number.isFinite(c.temperature)
        ? c.temperature
        : 0.2,
    timeoutSecs:
      typeof c.timeoutSecs === "number" && Number.isFinite(c.timeoutSecs)
        ? c.timeoutSecs
        : 90,
  };
}

function normalizeEffective(e: EffectiveAi): EffectiveAi {
  return {
    enabled: Boolean(e.enabled),
    ready: Boolean(e.ready),
    baseUrl: e.baseUrl ?? "",
    baseUrlFromEnv: Boolean(e.baseUrlFromEnv),
    model: e.model ?? "",
    modelFromEnv: Boolean(e.modelFromEnv),
    apiKeyMasked: e.apiKeyMasked ?? "",
    apiKeyFromEnv: Boolean(e.apiKeyFromEnv),
    useEnv: Boolean(e.useEnv),
    configPath: e.configPath ?? "",
  };
}

function sameConfig(a: AiConfig, b: AiConfig): boolean {
  return (Object.keys(a) as Array<keyof AiConfig>).every((k) => a[k] === b[k]);
}

function fallbackCopy(text: string): boolean {
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

type TestState =
  | { ok: true; data: AiTestResult }
  | { ok: false; message: string };

/* ------------------------------------------------------------------ */
/* 数字输入：只在失焦 / 回车时提交，避免输入小数时被回写打断              */
/* ------------------------------------------------------------------ */

function NumField(props: {
  id: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onCommit: (v: number) => void;
}) {
  const [text, setText] = useState(String(props.value));

  useEffect(() => {
    setText(String(props.value));
  }, [props.value]);

  const commit = (raw: string) => {
    const n = Number(raw);
    if (raw.trim() === "" || !Number.isFinite(n)) {
      setText(String(props.value));
      return;
    }
    const clamped = Math.min(props.max, Math.max(props.min, n));
    props.onCommit(clamped);
    setText(String(clamped));
  };

  return (
    <input
      id={props.id}
      type="number"
      inputMode="decimal"
      min={props.min}
      max={props.max}
      step={props.step}
      value={text}
      onChange={(e) => setText(e.target.value)}
      onBlur={(e) => commit(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit((e.target as HTMLInputElement).value);
      }}
    />
  );
}

/* ------------------------------------------------------------------ */
/* 主组件                                                              */
/* ------------------------------------------------------------------ */

export function SettingsView({ onSaved }: { onSaved: () => void }) {
  const toast = useContext(ToastCtx);

  const [cfg, setCfg] = useState<AiConfig | null>(null);
  const [saved, setSaved] = useState<AiConfig | null>(null);
  const [effective, setEffective] = useState<EffectiveAi | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const [saving, setSaving] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testState, setTestState] = useState<TestState | null>(null);

  const [models, setModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [modelsErrorAuto, setModelsErrorAuto] = useState(false);
  /** 请求序号：参数变化时让在途的旧请求作废，避免旧列表覆盖新列表。 */
  const modelsReqId = useRef(0);

  const [showKey, setShowKey] = useState(false);
  /** 用户主动点击了「自定义」：不套用预设，使用下方手动填写的 Base URL / 模型。 */
  const [customSelected, setCustomSelected] = useState(false);
  const baseUrlInputRef = useRef<HTMLInputElement>(null);

  const [probe, setProbe] = useState<Record<string, EnvProbe> | null>(null);
  const [probing, setProbing] = useState(false);
  const [probeError, setProbeError] = useState<string | null>(null);

  const envNames = cfg
    ? uniqueNames([cfg.envApiKeyName, cfg.envBaseUrlName, cfg.envModelName])
    : [];
  const envNamesKey = envNames.join("|");
  const cfgKey = cfg ? JSON.stringify(cfg) : "";

  /* ---------------- 加载 ---------------- */

  const load = async () => {
    setLoadError(null);
    try {
      const [c, e] = await Promise.all([
        api.getAiConfig(),
        api.resolveAiEffective().catch(() => null),
      ]);
      const next = normalizeConfig(c);
      setCfg(next);
      setSaved(next);
      setEffective(e ? normalizeEffective(e) : null);
    } catch (err) {
      setLoadError(errText(err));
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const refreshEffective = () => {
    api
      .resolveAiEffective()
      .then((e) => setEffective(normalizeEffective(e)))
      .catch(() => setEffective(null));
  };

  /* ---------------- 环境变量探测（防抖 300ms） ---------------- */

  useEffect(() => {
    if (!cfg?.useEnv) {
      setProbe(null);
      setProbeError(null);
      setProbing(false);
      return;
    }
    if (envNames.length === 0) {
      setProbe(null);
      setProbeError(null);
      setProbing(false);
      return;
    }
    let alive = true;
    setProbe(null);
    setProbeError(null);
    setProbing(true);
    const timer = window.setTimeout(async () => {
      try {
        const result = await api.probeEnvVars(envNames);
        if (alive) setProbe(result);
      } catch (err) {
        if (alive) {
          setProbe({});
          setProbeError(errText(err));
        }
      } finally {
        if (alive) setProbing(false);
      }
    }, 300);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cfg?.useEnv, envNamesKey]);

  const probeNow = async () => {
    if (!cfg) return;
    const names = uniqueNames([
      cfg.envApiKeyName,
      cfg.envBaseUrlName,
      cfg.envModelName,
    ]);
    if (names.length === 0) return;
    setProbing(true);
    setProbeError(null);
    try {
      setProbe(await api.probeEnvVars(names));
    } catch (err) {
      setProbe({});
      setProbeError(errText(err));
    } finally {
      setProbing(false);
    }
  };

  /* ---------------- 测试结果随配置变化清空 ---------------- */

  useEffect(() => {
    setTestState(null);
  }, [cfgKey]);

  /* ---------------- 派生状态 ---------------- */

  const dirty = Boolean(cfg && saved && !sameConfig(cfg, saved));
  const matchedPreset = cfg
    ? PRESETS.find((p) => normalizeBase(p.base) === normalizeBase(cfg.baseUrl)) ?? null
    : null;
  /** 当前选中的服务商卡片：主动自定义优先，否则按 Base URL 自动匹配。 */
  const selectedProvider = customSelected || !matchedPreset ? "custom" : matchedPreset.key;

  // 云端服务通常必须填写 API Key；本地服务（Ollama / LM Studio）可留空。
  const resolvedBaseText = (effective?.baseUrl || cfg?.baseUrl || "").trim();
  const isLocalEndpoint = (() => {
    const s = resolvedBaseText.toLowerCase();
    return [
      "http://localhost",
      "https://localhost",
      "http://127.0.0.1",
      "https://127.0.0.1",
      "http://0.0.0.0",
      "https://0.0.0.0",
      "http://[::1]",
      "localhost:",
      "127.0.0.1:",
      "0.0.0.0:",
      "[::1]:",
    ].some((prefix) => s.startsWith(prefix));
  })();
  const envKeyFound =
    Boolean(cfg?.useEnv) &&
    Boolean(cfg?.envApiKeyName.trim()) &&
    Boolean(probe?.[cfg!.envApiKeyName.trim()]?.found);
  const hasApiKey = cfg
    ? cfg.useEnv
      ? envKeyFound || Boolean(effective?.apiKeyMasked)
      : Boolean(cfg.apiKey.trim())
    : false;
  const keyMissing = Boolean(cfg?.enabled && !hasApiKey && !isLocalEndpoint);

  const statusKind = !effective
    ? "pending"
    : effective.enabled
      ? effective.ready
        ? keyMissing
          ? "warn"
          : "on"
        : "warn"
      : "off";
  const statusLabel = !effective
    ? cfg || loadError
      ? "状态未知"
      : "读取中…"
    : effective.enabled
      ? effective.ready
        ? keyMissing
          ? "已启用 · 缺少 API Key"
          : "已就绪"
        : "已启用但配置不完整"
      : "未启用";

  /* ---------------- 模型列表：随连接参数自动获取（防抖 800ms） ---------------- */

  useEffect(() => {
    // 参数变化：让在途的旧请求作废，并先清掉旧列表，避免展示过期的模型。
    modelsReqId.current += 1;
    setModels([]);
    setModelsError(null);
    setModelsErrorAuto(false);
    if (!cfg) return;
    const base = cfg.baseUrl.trim();
    // 静态 Base URL 与“环境变量里的 Base URL”都没配置时无从请求。
    if (!base && !(cfg.useEnv && cfg.envBaseUrlName.trim())) return;
    // 云端服务没 Key 时自动请求必然 401，等填 Key 后再自动获取（本地服务无此限制）。
    if (!isLocalEndpoint && !hasApiKey) return;
    // 用户还在输入半截地址（如 “http”）时不请求。
    if (!cfg.useEnv && !looksLikeEndpoint(base)) return;
    const timer = window.setTimeout(() => {
      void fetchModels(true);
    }, 800);
    return () => window.clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    cfg?.baseUrl,
    cfg?.apiKey,
    cfg?.useEnv,
    cfg?.envApiKeyName,
    cfg?.envBaseUrlName,
    cfg?.envModelName,
    isLocalEndpoint,
    hasApiKey,
  ]);

  /* ---------------- 动作 ---------------- */

  const applyPreset = (p: Preset) => {
    if (!cfg) return;
    if (p.custom) {
      setCustomSelected(true);
      if (!customSelected) {
        toast.push("已切换到自定义：在下方填写 Base URL 与模型即可", "info");
      }
      // 滚动并聚焦到 Base URL，方便直接输入自定义地址。
      window.requestAnimationFrame(() => {
        const el = baseUrlInputRef.current;
        if (!el) return;
        el.scrollIntoView({ block: "center", behavior: "smooth" });
        el.focus();
        el.select();
      });
      return;
    }
    setCustomSelected(false);
    setCfg({
      ...cfg,
      baseUrl: p.base,
      model: p.model,
      envApiKeyName: p.envKey,
      envBaseUrlName: p.envBase,
      envModelName: p.envModel,
    });
    toast.push(`已套用 ${p.label} 预设，记得保存`, "info");
  };

  const fetchModels = async (auto = false) => {
    if (!cfg) return;
    const reqId = ++modelsReqId.current;
    setModelsLoading(true);
    setModelsError(null);
    setModelsErrorAuto(false);
    try {
      const list = await api.listAiModels(cfg);
      if (reqId !== modelsReqId.current) return;
      const sorted = [...list].sort((a, b) => a.localeCompare(b));
      setModels(sorted);
      if (sorted.length === 0) {
        setModelsError("服务端没有返回可用模型");
        setModelsErrorAuto(auto);
      } else if (auto && !cfg.model.trim()) {
        // 自动获取到列表且模型为空（如 LM Studio）：自动选中第一个，省一步操作。
        setCfg((prev) =>
          prev && !prev.model.trim() ? { ...prev, model: sorted[0] } : prev,
        );
      }
    } catch (err) {
      if (reqId !== modelsReqId.current) return;
      setModels([]);
      setModelsError(errText(err));
      setModelsErrorAuto(auto);
    } finally {
      if (reqId === modelsReqId.current) setModelsLoading(false);
    }
  };

  const test = async () => {
    if (!cfg || testing) return;
    setTesting(true);
    setTestState(null);
    try {
      const data = await api.testAiConnection(cfg);
      setTestState({ ok: true, data });
    } catch (err) {
      setTestState({ ok: false, message: errText(err) });
    } finally {
      setTesting(false);
    }
  };

  const save = async () => {
    if (!cfg || saved === null || !dirty || saving) return;
    setSaving(true);
    try {
      await api.saveAiConfig(cfg);
      setSaved(cfg);
      // 后端保存时会 trim / 夹取值，回读一次让表单与磁盘上的值保持一致。
      try {
        const fresh = normalizeConfig(await api.getAiConfig());
        setCfg(fresh);
        setSaved(fresh);
      } catch {
        /* 保存已成功，回读失败不影响结果 */
      }
      toast.push("已保存", "ok");
      refreshEffective();
      onSaved();
    } catch (err) {
      toast.push("保存失败: " + errText(err), "err");
    } finally {
      setSaving(false);
    }
  };

  const revert = () => {
    if (!saved) return;
    setCfg(saved);
    setTestState(null);
    toast.push("已还原到上次保存的配置", "info");
  };

  const copyPath = async () => {
    const p = effective?.configPath ?? "";
    if (!p) {
      toast.push("配置路径未知，无法复制", "err");
      return;
    }
    let ok = false;
    try {
      await navigator.clipboard.writeText(p);
      ok = true;
    } catch {
      ok = fallbackCopy(p);
    }
    toast.push(ok ? "已复制配置路径" : "复制失败，请手动选择路径", ok ? "ok" : "err");
  };

  /* Ctrl+S 保存：用 ref 持有最新 save，避免频繁重绑监听 */
  const saveRef = useRef<() => void>(() => {});
  useEffect(() => {
    saveRef.current = () => {
      void save();
    };
  });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        saveRef.current();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  /* ---------------- 渲染辅助 ---------------- */

  const envKeyPh = matchedPreset
    ? matchedPreset.envKey || "（本地服务可留空）"
    : "例如 DEEPSEEK_API_KEY";
  const envBasePh = matchedPreset
    ? matchedPreset.envBase || "可选"
    : "例如 DEEPSEEK_BASE_URL";
  const envModelPh = matchedPreset
    ? matchedPreset.envModel || "可选"
    : "例如 DEEPSEEK_MODEL";

  const testLower =
    testState && !testState.ok ? testState.message.toLowerCase() : "";
  const testHints = [
    {
      key: "auth",
      text: "401 / 认证失败：检查 API Key 是否正确、是否过期或账户余额不足。",
      hit: /401|403|unauthorized|forbidden|auth|api.?key|invalid.*key|incorrect/.test(
        testLower,
      ),
    },
    {
      key: "url",
      text: "404 / 地址错误：检查 Base URL 是否指向 OpenAI 兼容接口（通常以 /v1 结尾）。",
      hit: /404|base.?url|endpoint|no such host|not found/.test(testLower),
    },
    {
      key: "net",
      text: "超时或连接失败：检查网络与代理，或确认本地服务（Ollama / LM Studio）已启动。",
      hit: /timeout|timed out|超时|connect|connection|refused|network|dns|fetch|无法连接|连接失败|检查网络|代理/.test(
        testLower,
      ),
    },
    {
      key: "model",
      text: "模型不可用：确认模型名完全一致，或从自动获取的模型列表中选择。",
      hit: /model|模型/.test(testLower),
    },
  ];

  const probeRows = cfg
    ? [
        { label: "API Key", name: cfg.envApiKeyName.trim() },
        { label: "Base URL", name: cfg.envBaseUrlName.trim() },
        { label: "Model", name: cfg.envModelName.trim() },
      ]
    : [];

  const configPathText = effective?.configPath || "";

  /* ---------------- 渲染 ---------------- */

  return (
    <>
      <div className="topbar">
        <span className="h">AI 配置</span>
        <div className="spacer" />
        <span className={"ai-status " + statusKind} aria-live="polite">
          <span className="dot" aria-hidden />
          {statusLabel}
        </span>
      </div>

      <div className="content">
        <div className="settings-page">
          {loadError ? (
            <div className="settings-card settings-error" role="alert">
              <div className="card-head">
                <span className="card-title">加载失败</span>
              </div>
              <div className="hint mono">{loadError}</div>
              <div className="actions-row">
                <button type="button" onClick={() => void load()}>
                  重新加载
                </button>
              </div>
            </div>
          ) : !cfg ? (
            <div className="settings-card muted">加载中…</div>
          ) : (
            <>
              <div className="settings-hero">
                <p className="settings-sub">
                  配置 AI 服务后，CleanSweep 会用它分析软件用途、提示卸载风险并给残留项分级。
                  API Key 仅保存在本机，不会上传。
                </p>
              </div>

              {/* 1. 启用 */}
              <section className="settings-card">
                <div className="card-head">
                  <span className="card-title">启用</span>
                </div>
                <div className="switch-row">
                  <button
                    type="button"
                    role="switch"
                    aria-checked={cfg.enabled}
                    aria-label="启用 AI 分析"
                    className={"switch" + (cfg.enabled ? " on" : "")}
                    onClick={() => setCfg({ ...cfg, enabled: !cfg.enabled })}
                  >
                    <span className="knob" aria-hidden />
                  </button>
                  <div className="switch-text">
                    <div className="switch-label">启用 AI 分析</div>
                    <div className="hint">
                      关闭后不会发起任何 AI 请求；已保存的 Key 与配置不会被清除。
                    </div>
                  </div>
                </div>
                {keyMissing && (
                  <div className="settings-warn" role="status">
                    尚未填写 API Key：当前服务商（{resolvedBaseText || "未设置"}）通常需要 Key
                    才能调用；Ollama / LM Studio 等本地服务可留空。
                  </div>
                )}
              </section>

              {/* 2. 服务商预设 */}
              <section className="settings-card">
                <div className="card-head">
                  <span className="card-title">服务商预设</span>
                  <span className="card-hint">
                    填入 Base URL、默认模型与环境变量名；不会清除已填写的 API Key
                  </span>
                </div>
                <div className="provider-grid">
                  {PRESET_CARDS.map((p) => {
                    const selected = p.custom
                      ? selectedProvider === "custom"
                      : selectedProvider === p.key;
                    return (
                      <button
                        key={p.key}
                        type="button"
                        className={"provider-card" + (selected ? " selected" : "")}
                        aria-pressed={selected}
                        title={
                          p.custom
                            ? "不套用预设，在下方手动填写 Base URL 与模型"
                            : p.base
                        }
                        onClick={() => applyPreset(p)}
                      >
                        {selected && <span className="current-tag">当前</span>}
                        <span className="p-label">{p.label}</span>
                        <span className="p-base">
                          {p.custom ? "在下方手动填写" : truncMiddle(p.base)}
                        </span>
                        <span className="p-model">
                          {p.custom
                            ? "不套用预设模型"
                            : p.model
                              ? truncMiddle(p.model, 32)
                              : "模型需自行获取"}
                        </span>
                      </button>
                    );
                  })}
                </div>
                {selectedProvider === "custom" && (
                  <div className="hint" style={{ marginTop: 8 }}>
                    自定义模式：使用下方「连接」中填写的 Base URL 与模型（保存后生效）。
                  </div>
                )}
              </section>

              {/* 3. 连接 */}
              <section className="settings-card">
                <div className="card-head">
                  <span className="card-title">连接</span>
                  <span className="card-hint">兼容 OpenAI 接口的云端或本地服务</span>
                </div>

                <div className="field">
                  <label className="label" htmlFor="ai-base-url">
                    Base URL
                  </label>
                  <input
                    id="ai-base-url"
                    ref={baseUrlInputRef}
                    className="mono"
                    value={cfg.baseUrl}
                    spellCheck={false}
                    autoComplete="off"
                    placeholder="https://api.deepseek.com/v1"
                    onChange={(e) => setCfg({ ...cfg, baseUrl: e.target.value })}
                  />
                  <div className="hint">
                    可省略 https:// 与 /v1，保存时会自动补全。
                  </div>
                </div>

                <div className="field">
                  <label className="label" htmlFor="ai-model">
                    模型
                  </label>
                  <div className="model-row">
                    <input
                      id="ai-model"
                      className="mono"
                      value={cfg.model}
                      spellCheck={false}
                      autoComplete="off"
                      placeholder="deepseek-flash"
                      onChange={(e) => setCfg({ ...cfg, model: e.target.value })}
                    />
                    <button
                      type="button"
                      onClick={() => void fetchModels(false)}
                      disabled={modelsLoading}
                      aria-busy={modelsLoading}
                    >
                      {modelsLoading ? (
                        <span className="spinner" />
                      ) : models.length > 0 ? (
                        "刷新"
                      ) : (
                        "获取模型"
                      )}
                    </button>
                  </div>
                  {modelsLoading && (
                    <div className="hint">正在获取模型列表…</div>
                  )}
                  {modelsError && (
                    <div className="hint err-hint">
                      {modelsErrorAuto ? "未能自动获取模型列表" : "获取模型失败"}：
                      {modelsError}（仍可手动输入）
                    </div>
                  )}
                  {models.length > 0 && (
                    <div className="model-list">
                      <label className="hint" htmlFor="ai-model-select">
                        已获取 {models.length} 个模型，可直接选择：
                      </label>
                      <select
                        id="ai-model-select"
                        value={models.includes(cfg.model) ? cfg.model : ""}
                        onChange={(e) => {
                          if (e.target.value) {
                            setCfg({ ...cfg, model: e.target.value });
                          }
                        }}
                      >
                        <option value="">从列表中选择…</option>
                        {models.map((m) => (
                          <option key={m} value={m}>
                            {m}
                          </option>
                        ))}
                      </select>
                    </div>
                  )}
                  <div className="hint">
                    {!isLocalEndpoint && !hasApiKey
                      ? "填写 API Key 后会自动获取模型列表；也可手动填写模型名（区分大小写）。"
                      : "模型列表会随 Base URL / Key 自动获取；可点「刷新」重试，也可手动填写（区分大小写）。"}
                  </div>
                </div>

                <div className="field">
                  <label className="label" htmlFor="ai-api-key">
                    API Key
                  </label>
                  <div className="model-row">
                    <input
                      id="ai-api-key"
                      type={showKey ? "text" : "password"}
                      className="mono"
                      value={cfg.apiKey}
                      spellCheck={false}
                      autoComplete="off"
                      placeholder="sk-…（无需 Key 的本地服务可留空）"
                      onChange={(e) => setCfg({ ...cfg, apiKey: e.target.value })}
                    />
                    <button
                      type="button"
                      onClick={() => setShowKey((v) => !v)}
                      aria-pressed={showKey}
                      aria-label={showKey ? "隐藏 API Key" : "显示 API Key"}
                    >
                      {showKey ? "隐藏" : "显示"}
                    </button>
                  </div>
                  <div className="hint">
                    仅保存在本机配置文件
                    {configPathText ? (
                      <>
                        （<span className="mono">{configPathText}</span>）
                      </>
                    ) : null}
                    ，不会上传；无需 Key 的本地服务可留空。
                  </div>
                  {!cfg.apiKey && effective?.apiKeyMasked && (
                    <div className="hint">
                      当前已保存静态 Key：
                      <span className="mono">{effective.apiKeyMasked}</span>
                    </div>
                  )}
                </div>

                <details className="settings-advanced">
                  <summary>高级参数</summary>
                  <div className="field-grid">
                    <div className="field">
                      <label className="label" htmlFor="ai-temperature">
                        温度 temperature
                      </label>
                      <NumField
                        id="ai-temperature"
                        value={cfg.temperature}
                        min={0}
                        max={2}
                        step={0.1}
                        onCommit={(v) => setCfg({ ...cfg, temperature: v })}
                      />
                      <div className="hint">
                        0–2，默认 0.2；越大越发散，分析类任务建议保持较低值。
                      </div>
                    </div>
                    <div className="field">
                      <label className="label" htmlFor="ai-timeout">
                        超时（秒）
                      </label>
                      <NumField
                        id="ai-timeout"
                        value={cfg.timeoutSecs}
                        min={5}
                        max={600}
                        step={1}
                        onCommit={(v) => setCfg({ ...cfg, timeoutSecs: v })}
                      />
                      <div className="hint">
                        5–600，默认 90；本地模型或弱网可适当调大。
                      </div>
                    </div>
                  </div>
                </details>
              </section>

              {/* 4. 读取方式 */}
              <section className="settings-card">
                <div className="card-head">
                  <span className="card-title">读取方式</span>
                </div>
                <div className="segmented" role="group" aria-label="配置读取方式">
                  <button
                    type="button"
                    className={!cfg.useEnv ? "active" : ""}
                    aria-pressed={!cfg.useEnv}
                    onClick={() => setCfg({ ...cfg, useEnv: false })}
                  >
                    静态值
                  </button>
                  <button
                    type="button"
                    className={cfg.useEnv ? "active" : ""}
                    aria-pressed={cfg.useEnv}
                    onClick={() => setCfg({ ...cfg, useEnv: true })}
                  >
                    环境变量优先（取不到回退静态值）
                  </button>
                </div>
                <div className="hint" style={{ marginTop: 8 }}>
                  {cfg.useEnv
                    ? "优先从环境变量读取 Base URL / API Key / 模型，缺失的项回退到上面的静态值。"
                    : "始终使用上面填写的静态值。"}
                </div>

                {cfg.useEnv && (
                  <>
                    <div className="env-fields">
                      <div className="field">
                        <label className="label" htmlFor="env-api-key">
                          API Key 变量名
                        </label>
                        <input
                          id="env-api-key"
                          className="mono"
                          value={cfg.envApiKeyName}
                          spellCheck={false}
                          autoComplete="off"
                          placeholder={envKeyPh}
                          onChange={(e) =>
                            setCfg({ ...cfg, envApiKeyName: e.target.value })
                          }
                        />
                      </div>
                      <div className="field">
                        <label className="label" htmlFor="env-base-url">
                          Base URL 变量名
                        </label>
                        <input
                          id="env-base-url"
                          className="mono"
                          value={cfg.envBaseUrlName}
                          spellCheck={false}
                          autoComplete="off"
                          placeholder={envBasePh}
                          onChange={(e) =>
                            setCfg({ ...cfg, envBaseUrlName: e.target.value })
                          }
                        />
                      </div>
                      <div className="field">
                        <label className="label" htmlFor="env-model">
                          Model 变量名
                        </label>
                        <input
                          id="env-model"
                          className="mono"
                          value={cfg.envModelName}
                          spellCheck={false}
                          autoComplete="off"
                          placeholder={envModelPh}
                          onChange={(e) =>
                            setCfg({ ...cfg, envModelName: e.target.value })
                          }
                        />
                      </div>
                    </div>

                    <div className="probe-head">
                      <span>环境变量探测</span>
                      <button
                        type="button"
                        className="ghost"
                        onClick={() => void probeNow()}
                        disabled={probing || envNames.length === 0}
                        aria-busy={probing}
                      >
                        {probing ? <span className="spinner" /> : "重新探测"}
                      </button>
                    </div>

                    <div className="probe-list" aria-live="polite">
                      {probeRows.map((row) => {
                        const p = row.name ? probe?.[row.name] : undefined;
                        const stateClass = !row.name
                          ? "missing"
                          : probing || probe === null
                            ? "pending"
                            : probeError
                              ? "missing"
                              : p?.found
                                ? "found"
                                : "missing";
                        return (
                          <div className="probe-row" key={row.label}>
                            <span className="probe-purpose">{row.label}</span>
                            <span className="probe-name" title={row.name}>
                              {row.name || "—"}
                            </span>
                            <span className={"probe-state " + stateClass}>
                              {!row.name ? (
                                "未填写"
                              ) : probing || probe === null ? (
                                <>
                                  <span className="spinner" /> 检测中…
                                </>
                              ) : probeError ? (
                                "探测失败"
                              ) : p?.found ? (
                                `已设置 · ${p.masked || "••••••"}`
                              ) : (
                                "未设置"
                              )}
                            </span>
                          </div>
                        );
                      })}
                    </div>
                    {probeError && (
                      <div className="hint err-hint">探测失败：{probeError}</div>
                    )}
                    <div className="hint" style={{ marginTop: 8 }}>
                      CleanSweep 只读取自身进程的环境变量；修改系统级环境变量后，可能需要重启
                      CleanSweep 才能读到。
                    </div>
                  </>
                )}
              </section>

              {/* 5. 测试 */}
              <section className="settings-card">
                <div className="card-head">
                  <span className="card-title">测试连接</span>
                  <span className="card-hint">
                    使用当前表单值（含未保存修改），不会保存或启用 AI
                  </span>
                </div>
                <button
                  type="button"
                  onClick={() => void test()}
                  disabled={testing}
                  aria-busy={testing}
                >
                  {testing ? <span className="spinner" /> : "测试连接"}
                </button>

                <div aria-live="polite">
                  {testing && (
                    <div className="test-result loading">
                      <span className="spinner" />
                      正在请求 {cfg.baseUrl || "…"} …
                    </div>
                  )}
                  {!testing && testState?.ok && (
                    <div className="test-result ok" role="status">
                      <div className="tr-head">
                        连接成功
                        <span className="tag">{testState.data.latencyMs} ms</span>
                      </div>
                      <div className="tr-msg">
                        模型 {testState.data.model} ·{" "}
                        {testState.data.message || "服务端返回正常"}
                      </div>
                    </div>
                  )}
                  {!testing && testState && !testState.ok && (
                    <div className="test-result err" role="alert">
                      <div className="tr-head">连接失败</div>
                      <div className="tr-msg">{testState.message}</div>
                      <ul className="test-hints">
                        {testHints.map((h) => (
                          <li key={h.key} className={h.hit ? "hit" : ""}>
                            {h.text}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                </div>
              </section>

              {/* 6. 当前生效 */}
              <section className="settings-card">
                <div className="card-head">
                  <span className="card-title">当前生效配置</span>
                  <span className="card-hint">
                    来自已保存配置，未保存的修改不会生效
                  </span>
                  <button type="button" className="ghost" onClick={refreshEffective}>
                    刷新
                  </button>
                </div>
                {effective ? (
                  <div className="kv settings-kv">
                    <span className="k">状态</span>
                    <span className="v">
                      {effective.enabled
                        ? effective.ready
                          ? "已启用 · 就绪"
                          : "已启用 · 配置不完整"
                        : "未启用"}
                    </span>
                    <span className="k">Base URL</span>
                    <span className="v mono">
                      {effective.baseUrl || "（未设置）"}
                      {effective.baseUrlFromEnv && <span className="tag">env</span>}
                    </span>
                    <span className="k">模型</span>
                    <span className="v mono">
                      {effective.model || "（未设置）"}
                      {effective.modelFromEnv && <span className="tag">env</span>}
                    </span>
                    <span className="k">API Key</span>
                    <span className="v mono">
                      {effective.apiKeyMasked || "（未设置）"}
                      {effective.apiKeyFromEnv && <span className="tag">env</span>}
                    </span>
                    <span className="k">读取方式</span>
                    <span className="v">
                      {effective.useEnv ? "环境变量优先（回退静态值）" : "静态值"}
                    </span>
                    <span className="k">配置文件</span>
                    <span className="v">
                      <span className="path-row">
                        <code title={configPathText}>
                          {configPathText || "（未知）"}
                        </code>
                        <button
                          type="button"
                          className="ghost"
                          onClick={() => void copyPath()}
                          disabled={!configPathText}
                          aria-label="复制配置路径"
                        >
                          复制
                        </button>
                      </span>
                    </span>
                  </div>
                ) : (
                  <div className="hint">暂无生效配置（读取失败或后端未返回）。</div>
                )}
              </section>

              {/* 7. 保存 / 还原 */}
              <div className="actions-row">
                {dirty && (
                  <span className="dirty-pill" role="status">
                    未保存的更改
                  </span>
                )}
                <div className="spacer" />
                <span className="settings-kbd" title="Ctrl+S 保存">Ctrl+S</span>
                <button
                  type="button"
                  className="ghost"
                  onClick={revert}
                  disabled={!dirty || saving}
                >
                  还原
                </button>
                <button
                  type="button"
                  className="primary"
                  onClick={() => void save()}
                  disabled={!dirty || saving}
                  aria-busy={saving}
                >
                  {saving ? <span className="spinner" /> : "保存"}
                </button>
              </div>

              {/* 8. 安全说明与数据范围 */}
              <details className="settings-details">
                <summary>安全说明与数据范围</summary>
                <div className="details-body">
                  <ul>
                    <li>运行卸载程序前会要求确认。</li>
                    <li>
                      删除残留（文件 / 目录 / 注册表项 / 服务 / 计划任务）前，会列出全部待删项并要求确认。
                    </li>
                    <li>AI 仅给出建议（可删 / 谨慎 / 保留），最终是否删除由你勾选并确认。</li>
                    <li>所有删除操作不可撤销。</li>
                  </ul>
                  <p>
                    数据范围：读取注册表 Uninstall 键、Store/MSIX 应用列表、Program Files /
                    LocalAppData / Roaming / ProgramData / 开始菜单，以及注册表 Software /
                    Services / Run / TaskCache 等；删除仅针对你确认的项目。AI 分析会向所配置的
                    服务发送软件名称、发布者、版本与残留路径等分析所需信息。
                  </p>
                </div>
              </details>
            </>
          )}
        </div>
      </div>
    </>
  );
}
