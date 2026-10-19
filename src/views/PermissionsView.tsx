import { useEffect, useState, useContext } from "react";
import {
  api,
  type InstalledApp,
  type FirewallRule,
  type CapPermission,
  type AutostartEntry,
} from "../bindings";
import { useConfirm } from "../components/Confirm";
import { ToastCtx } from "../components/Toast";

type Tab = "firewall" | "capability" | "autostart";

export function PermissionsView({ apps }: { apps: InstalledApp[] }) {
  const [tab, setTab] = useState<Tab>("firewall");
  return (
    <>
      <div className="topbar">
        <span className="title">应用权限</span>
        <div className="spacer" />
        <button className={tab === "firewall" ? "primary" : ""} onClick={() => setTab("firewall")}>联网</button>
        <button className={tab === "capability" ? "primary" : ""} onClick={() => setTab("capability")}>Store 能力</button>
        <button className={tab === "autostart" ? "primary" : ""} onClick={() => setTab("autostart")}>自启动</button>
      </div>
      <div className="content">
        {tab === "firewall" && <FirewallTab apps={apps} />}
        {tab === "capability" && <CapabilityTab apps={apps} />}
        {tab === "autostart" && <AutostartTab />}
      </div>
    </>
  );
}

function FirewallTab({ apps }: { apps: InstalledApp[] }) {
  const [selected, setSelected] = useState<InstalledApp | null>(null);
  const [candidates, setCandidates] = useState<string[]>([]);
  const [exe, setExe] = useState("");
  const [blocked, setBlocked] = useState(false);
  const [managed, setManaged] = useState<FirewallRule[]>([]);
  const [busy, setBusy] = useState(false);
  const confirm = useConfirm();
  const toast = useContext(ToastCtx);

  const loadManaged = () => api.firewallListManaged().then(setManaged);
  useEffect(() => { loadManaged(); }, []);

  const onPickApp = async (app: InstalledApp) => {
    setSelected(app);
    const cs = await api.guessMainExe(app);
    setCandidates(cs);
    setExe(cs[0] ?? "");
    setBlocked(cs[0] ? await api.firewallIsBlocked(cs[0]) : false);
  };

  const doBlock = async (block: boolean) => {
    if (!exe) return;
    const action = block ? "禁止联网" : "恢复联网";
    const ok = await confirm({
      title: `${action}：${exe}`,
      danger: block,
      message: (
        <div>
          <p>将{block ? "添加" : "移除"} Windows 防火墙规则，{block ? "阻止" : "恢复"}该程序的所有联网。</p>
          <p className="muted">规则名前缀 CleanSweep_Block_，可随时在此页恢复。</p>
        </div>
      ),
      confirmLabel: block ? "禁止" : "恢复",
      cancelLabel: "取消",
    });
    if (!ok) return;
    setBusy(true);
    try {
      const msg = block ? await api.firewallBlock(exe) : await api.firewallUnblock(exe);
      toast.push(msg, "ok");
      setBlocked(await api.firewallIsBlocked(exe));
      await loadManaged();
    } catch (e: any) {
      toast.push(String(e?.message ?? e), "err");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="split" style={{ flexDirection: "column" }}>
      <div className="detail-col" style={{ maxWidth: "none", borderRight: "none" }}>
        <div className="section">
          <div className="h">防火墙 · 联网控制</div>
          <div className="sub">桌面应用：选软件后自动推断主 exe，也可手动粘贴。该程序所有出/入站流量将被阻止。</div>
          <div className="row" style={{ marginBottom: 8 }}>
            <select
              style={{ width: 360 }}
              value={selected?.id ?? ""}
              onChange={(e) => { const a = apps.find((x) => x.id === e.target.value); if (a) onPickApp(a); }}
            >
              <option value="">— 选择软件 —</option>
              {apps.map((a) => (
                <option key={a.id} value={a.id}>{a.displayName}{a.publisher ? ` · ${a.publisher}` : ""}</option>
              ))}
            </select>
            <select
              style={{ width: 380 }}
              value={exe}
              onChange={(e) => { setExe(e.target.value); api.firewallIsBlocked(e.target.value).then(setBlocked).catch(() => setBlocked(false)); }}
            >
              {candidates.length === 0 && <option value="">手动输入 exe 路径</option>}
              {candidates.map((c) => <option key={c} value={c}>{c}</option>)}
              {exe && !candidates.includes(exe) && <option value={exe}>{exe}</option>}
            </select>
          </div>
          <input
            placeholder="C:\Path\To\app.exe"
            value={exe}
            onChange={(e) => setExe(e.target.value)}
            onBlur={() => {
              if (exe) api.firewallIsBlocked(exe).then(setBlocked).catch(() => setBlocked(false));
            }}
            style={{ marginBottom: 10, fontFamily: "var(--font-mono)", fontSize: 12 }}
          />
          <div className="row">
            <button className="danger" disabled={!exe || busy || blocked} onClick={() => doBlock(true)}>禁止联网</button>
            <button disabled={!exe || busy || !blocked} onClick={() => doBlock(false)}>恢复联网</button>
            {blocked && <span className="pill danger">已禁止</span>}
            {busy && <span className="spinner" />}
          </div>
        </div>

        <div className="section">
          <div className="h">本工具管理的规则 · {managed.length}</div>
          {managed.length === 0 && <div className="muted">暂无规则。</div>}
          {managed.map((r) => (
            <div key={r.name} className="res-row" style={{ gridTemplateColumns: "1fr auto" }}>
              <div>
                <div className="path">{r.exePath}</div>
                <div className="meta">
                  {r.name} · {r.directionIn ? "入站" : ""}{r.directionIn && r.directionOut ? " + " : ""}{r.directionOut ? "出站" : ""} · {r.enabled ? "启用" : "禁用"}
                </div>
              </div>
              <button
                className="ghost"
                disabled={!r.exePath}
                title={r.exePath ? "" : "无法从规则中解析程序路径"}
                onClick={async () => {
                  const ok = await confirm({
                    title: `恢复 ${r.exePath || r.name} 联网？`,
                    message: <div>将移除防火墙阻止规则 <code>{r.name}</code>。</div>,
                    confirmLabel: "恢复",
                  });
                  if (!ok) return;
                  try {
                    await api.firewallUnblock(r.exePath);
                    toast.push("已恢复", "ok");
                    await loadManaged();
                  } catch (e: any) { toast.push(String(e?.message ?? e), "err"); }
                }}
              >恢复</button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function CapabilityTab({ apps }: { apps: InstalledApp[] }) {
  const [pkg, setPkg] = useState<InstalledApp | null>(null);
  const [caps, setCaps] = useState<[string, string][]>([]);
  const [perms, setPerms] = useState<CapPermission[]>([]);
  const [busy, setBusy] = useState(false);
  const toast = useContext(ToastCtx);

  useEffect(() => { api.listCapabilities().then(setCaps); }, []);

  const loadPerms = async (p: InstalledApp) => {
    setPkg(p);
    const family = p.packageFamily || extractFamilyName(p);
    if (!family) { setPerms([]); return; }
    setPerms(await api.listPackageCapabilities(family));
  };

  const setVal = async (cap: string, value: string) => {
    const family = pkg ? pkg.packageFamily || extractFamilyName(pkg) : null;
    if (!family) return;
    setBusy(true);
    try {
      const msg = await api.setCapability(cap, family, value);
      toast.push(msg, "ok");
      await loadPerms(pkg!);
    } catch (e: any) { toast.push(String(e?.message ?? e), "err"); }
    finally { setBusy(false); }
  };

  const storeApps = apps.filter((a) => a.source === "store");

  return (
    <div className="detail-col" style={{ maxWidth: "none" }}>
      <div className="section">
        <div className="h">Store / UWP 能力限制</div>
        <div className="sub">仅对 Store/UWP 应用有效。通过 ConsentStore 注册表控制摄像头/麦克风/位置等能力：允许 / 拒绝 / 询问。</div>
        <select
          style={{ width: 480, marginBottom: 4 }}
          value={pkg?.id ?? ""}
          onChange={(e) => { const a = storeApps.find((x) => x.id === e.target.value); if (a) loadPerms(a); }}
        >
          <option value="">— 选择 Store 应用 —</option>
          {storeApps.map((a) => (
            <option key={a.id} value={a.id}>{a.displayName}{a.version ? ` · ${a.version}` : ""}</option>
          ))}
        </select>
      </div>
      {pkg && (
        <div className="section">
          <div className="h">{pkg.displayName}</div>
          {perms.length === 0 ? (
            <div className="muted">该应用没有可限制的能力（或 PackageFamilyName 无法解析）。</div>
          ) : (
            <div>
              {perms.map((p) => (
                <div key={p.capability} className="res-row" style={{ gridTemplateColumns: "1fr auto" }}>
                  <div>
                    <div>{p.label}</div>
                    <div className="meta">
                      {p.capability} · {p.value ?? "未设置"}
                    </div>
                  </div>
                  <div className="row" style={{ gap: 4 }}>
                    {(["Allow", "Prompt", "Deny"] as const).map((v) => (
                      <button
                        key={v}
                        className={p.value === v ? "primary" : ""}
                        disabled={busy}
                        onClick={() => setVal(p.capability, v)}
                        style={{ padding: "4px 10px", fontSize: 12 }}
                      >
                        {v === "Allow" ? "允许" : v === "Deny" ? "拒绝" : "询问"}
                      </button>
                    ))}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function extractFamilyName(app: InstalledApp): string | null {
  const us = app.uninstallString ?? app.quietUninstallString ?? "";
  const i = us.indexOf("-Package '");
  if (i < 0) return null;
  const start = i + "-Package '".length;
  const end = us.indexOf("'", start);
  if (end < 0) return null;
  const full = us.slice(start, end);
  // PackageFullName = Name_Version_Architecture_ResourceId_PublisherId
  // PackageFamilyName = Name_PublisherId（仅作为旧后端兜底；新后端直接返回 packageFamily）
  const parts = full.split("_");
  if (parts.length < 2) return full;
  return `${parts[0]}_${parts[parts.length - 1]}`;
}

function AutostartTab() {
  const [items, setItems] = useState<AutostartEntry[]>([]);
  const [filter, setFilter] = useState("");
  const [busy, setBusy] = useState(false);
  const confirm = useConfirm();
  const toast = useContext(ToastCtx);

  const load = () => {
    setBusy(true);
    api.listAutostart(filter.trim() || null).then(setItems).finally(() => setBusy(false));
  };
  useEffect(() => { load(); /* eslint-disable-next-line */ }, []);

  const act = async (entry: AutostartEntry, fn: (e: AutostartEntry) => Promise<string>, label: string, danger = false) => {
    const ok = await confirm({
      title: `${label}：${entry.name}`,
      danger,
      message: (
        <div>
          <p>类型：{entry.kind === "registry" ? "注册表自启项" : "计划任务"}</p>
          {entry.value && <pre>{entry.value}</pre>}
          <p className="muted">位置：{entry.location}</p>
        </div>
      ),
      confirmLabel: label,
    });
    if (!ok) return;
    try {
      const msg = await fn(entry);
      toast.push(msg, "ok");
      await load();
    } catch (e: any) { toast.push(String(e?.message ?? e), "err"); }
  };

  return (
    <div className="detail-col" style={{ maxWidth: "none" }}>
      <div className="section">
        <div className="h">自启动项</div>
        <div className="sub">扫描注册表 Run/RunOnce 键与计划任务。可禁用（注册表项备份 disabled_ 前缀，计划任务用 /DISABLE）或永久删除。</div>
        <div className="row" style={{ marginBottom: 10 }}>
          <input
            placeholder="按软件名过滤（留空显示全部）"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && load()}
          />
          <button onClick={load} disabled={busy}>{busy ? <span className="spinner" /> : "扫描"}</button>
        </div>
        {items.length === 0 ? (
          <div className="muted">无自启动项。</div>
        ) : (
          <div>
            {items.map((it) => (
              <div key={it.id} className="res-row" style={{ gridTemplateColumns: "1fr auto auto" }}>
                <div>
                  <div className="path">{it.name}</div>
                  {it.value && <div className="meta" style={{ wordBreak: "break-all" }}>{it.value}</div>}
                  <div className="meta">
                    <span className="tag">{it.kind}</span> {it.location}
                    {it.disabled && <span className="pill warn" style={{ marginLeft: 6 }}>已禁用</span>}
                  </div>
                </div>
                <div className="row" style={{ gap: 4 }}>
                  {it.disabled ? (
                    <button onClick={() => act(it, api.enableAutostart, "启用")}>启用</button>
                  ) : (
                    <button onClick={() => act(it, api.disableAutostart, "禁用")}>禁用</button>
                  )}
                  <button className="danger" onClick={() => act(it, api.removeAutostart, "删除", true)}>删除</button>
                </div>
                <div />
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
