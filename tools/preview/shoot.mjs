/**
 * Dev-only visual QA: drives headless Edge over CDP and captures screenshots of
 * the browser preview (tools/preview). Requires the mock vite server on :5199.
 *
 * Usage: node tools/preview/shoot.mjs [--port 9222] [--shot-dir tools/preview/shots]
 * Node 22+ (global WebSocket / fetch).
 */
import { spawn, spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "../..");

const args = process.argv.slice(2);
const arg = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
};
const CDP_PORT = Number(arg("port", "9222"));
const SHOT_DIR = path.resolve(repo, arg("shot-dir", "tools/preview/shots"));
const APP_URL = arg("url", "http://127.0.0.1:5199/tools/preview/index.html");
const EDGE = arg("edge", "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class CDP {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener("message", (ev) => {
      let msg;
      try {
        msg = JSON.parse(typeof ev.data === "string" ? ev.data : ev.data.toString());
      } catch {
        return;
      }
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(`${msg.error.message} (${msg.error.code})`)) : resolve(msg.result);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
}

async function waitFor(label, fn, timeoutMs = 20000) {
  const start = Date.now();
  for (;;) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {
      /* retry */
    }
    if (Date.now() - start > timeoutMs) throw new Error(`timeout waiting for ${label}`);
    await sleep(200);
  }
}

async function newPage(jsUrl) {
  // Chrome >=111 requires PUT for /json/new.
  const res = await fetch(`http://127.0.0.1:${CDP_PORT}/json/new?${encodeURIComponent(jsUrl)}`, {
    method: "PUT",
  });
  return res.json();
}

function clickButtonByText(text) {
  return `(() => {
    const btn = [...document.querySelectorAll('button')].find((b) => (b.textContent || '').includes(${JSON.stringify(text)}));
    if (!btn) return 'NOT_FOUND';
    btn.click();
    return 'CLICKED';
  })()`;
}

async function main() {
  if (!existsSync(EDGE)) throw new Error(`Edge not found: ${EDGE}`);
  await mkdir(SHOT_DIR, { recursive: true });

  // Keep the browser profile outside the repo: vite watches the repo root and
  // Edge locks its cache files (EBUSY crashes the watcher).
  const userData = path.join(os.tmpdir(), "cleansweep-preview-edge");
  const edge = spawn(
    EDGE,
    [
      "--headless=new",
      "--disable-gpu",
      "--hide-scrollbars",
      "--no-first-run",
      "--no-default-browser-check",
      `--remote-debugging-port=${CDP_PORT}`,
      `--user-data-dir=${userData}`,
      "--window-size=1280,920",
      "about:blank",
    ],
    { stdio: "ignore" }
  );

  let page;
  let cdp;
  try {
    await waitFor("edge devtools", async () => (await fetch(`http://127.0.0.1:${CDP_PORT}/json/version`)).ok);
    page = await newPage(`${APP_URL}?state=static`);
    cdp = new CDP(new WebSocket(page.webSocketDebuggerUrl));
    await new Promise((resolve, reject) => {
      cdp.ws.addEventListener("open", resolve, { once: true });
      cdp.ws.addEventListener("error", reject, { once: true });
    });
    await cdp.send("Page.enable");
    await cdp.send("Runtime.enable");
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: 1280,
      height: 920,
      deviceScaleFactor: 1,
      mobile: false,
    });

    const setViewport = async (height) => {
      await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: 1280,
        height,
        deviceScaleFactor: 1,
        mobile: false,
      });
      await sleep(450);
    };

    const shot = async (name, fullPage = false) => {
      if (fullPage) {
        // The app scrolls inside .content (height:100vh), so a taller viewport is
        // the reliable way to capture the whole settings page.
        await setViewport(2600);
      }
      const { data } = await cdp.send("Page.captureScreenshot", { format: "png" });
      const file = path.join(SHOT_DIR, name);
      await writeFile(file, Buffer.from(data, "base64"));
      console.log("saved", path.relative(repo, file));
      if (fullPage) await setViewport(920);
    };

    const navigate = async (state) => {
      await cdp.send("Page.navigate", { url: `${APP_URL}?state=${encodeURIComponent(state)}` });
      await sleep(2400);
    };

    // 1) static config overview (viewport + full page)
    await navigate("static");
    await shot("settings-static.png");
    await shot("settings-static-full.png", true);

    // 2) test connection success (tall viewport so the result panel is visible)
    await navigate("static");
    await setViewport(1900);
    const clicked = await cdp.send("Runtime.evaluate", {
      expression: clickButtonByText("测试连接"),
      returnByValue: true,
    });
    console.log("click 测试连接:", clicked.result?.value);
    await sleep(1800);
    await shot("settings-test-ok.png");

    // 3) environment-variable mode
    await navigate("env");
    await setViewport(2400);
    await shot("settings-env.png");

    // 4) failed test connection
    await navigate("fail");
    await setViewport(1900);
    const clickedFail = await cdp.send("Runtime.evaluate", {
      expression: clickButtonByText("测试连接"),
      returnByValue: true,
    });
    console.log("click 测试连接 (fail):", clickedFail.result?.value);
    await sleep(1800);
    await shot("settings-test-fail.png");

    // 5) model list is fetched automatically (no click); verify it rendered
    await navigate("static");
    await setViewport(1900);
    const autoList = await cdp.send("Runtime.evaluate", {
      expression: `(() => {
        const s = document.getElementById('ai-model-select');
        return s ? 'AUTO_LIST_RENDERED:' + s.options.length : 'NO_AUTO_LIST';
      })()`,
      returnByValue: true,
    });
    console.log("auto model list:", autoList.result?.value);
    await shot("settings-models.png");

    // 6) scrolled to the bottom of the page (footer + effective config)
    await navigate("static");
    await setViewport(920);
    await cdp.send("Runtime.evaluate", {
      expression: `(() => { const c = document.querySelector('.content'); if (c) c.scrollTo(0, 999999); return 'scrolled'; })()`,
      returnByValue: true,
    });
    await sleep(600);
    await shot("settings-bottom.png");

    // 7) 用户真实场景：已启用但没填 API Key
    await navigate("nokey");
    await shot("settings-no-key.png");

    const click = async (text) => {
      const r = await cdp.send("Runtime.evaluate", {
        expression: clickButtonByText(text),
        returnByValue: true,
      });
      await sleep(400);
      return r.result?.value;
    };

    // 7b) 自定义服务商：点击后应聚焦 Base URL 并显示自定义提示
    await cdp.send("Page.navigate", { url: `${APP_URL}?state=static` });
    await setViewport(1900);
    await sleep(1600);
    console.log("custom preset:", await click("自定义"));
    await sleep(700);
    await shot("settings-custom-mode.png");

    // 8) uninstall wizard: AI pre-check → uninstall → AI residual verdict → post-check
    await cdp.send("Page.navigate", { url: `${APP_URL}?view=wizard&state=wizard` });
    await setViewport(1000);
    await sleep(2400);
    await shot("wizard-intro.png");
    // 默认展开的范围说明 + 勾选“我已了解扫描/清理范围”，才能开始卸载
    const ack = await cdp.send("Runtime.evaluate", {
      expression: `(() => {
        const cb = document.querySelector('.scope-ack input[type=checkbox]');
        if (!cb) return 'NO_ACK';
        cb.click();
        return 'ACKED';
      })()`,
      returnByValue: true,
    });
    console.log("scope ack:", ack.result?.value);
    await sleep(400);
    await shot("wizard-scope.png");

    console.log("wizard start:", await click("开始卸载"));
    // confirm dialog
    console.log("wizard confirm:", await click("运行卸载"));
    await sleep(1400);
    console.log("wizard scan:", await click("扫描残留并让 AI 判断是否干净"));
    await sleep(2600);
    await shot("wizard-review.png");
    console.log("wizard select safe:", await click("勾选 AI 建议可删项"));
    console.log("wizard delete:", await click("删除选中"));
    console.log("wizard confirm delete:", await click("全部删除"));
    await sleep(2600);
    await shot("wizard-done.png");

    // 9) software list + detail + AI analysis
    await cdp.send("Page.navigate", { url: `${APP_URL}?view=apps&state=static` });
    await setViewport(920);
    await sleep(1800);
    await shot("apps-list.png");
    await cdp.send("Runtime.evaluate", {
      expression: `(() => { const r = document.querySelector('.list-row'); if (!r) return 'NO_ROW'; r.click(); return 'CLICKED'; })()`,
      returnByValue: true,
    });
    await sleep(900);
    await shot("apps-detail.png");
    console.log("apps AI:", await click("AI 分析用途与风险"));
    await sleep(1800);
    await shot("apps-ai.png");

    // 10) permissions tabs
    await cdp.send("Page.navigate", { url: `${APP_URL}?view=permissions&state=static` });
    await sleep(1600);
    const appSelect = await cdp.send("Runtime.evaluate", {
      expression: `(() => {
        const sels = [...document.querySelectorAll('select')];
        const s = sels.find((x) => [...x.options].some((o) => (o.textContent || '').includes('Google Chrome')));
        if (!s) return 'NO_SELECT';
        const opt = [...s.options].find((o) => (o.textContent || '').includes('Google Chrome'));
        s.value = opt.value;
        s.dispatchEvent(new Event('change', { bubbles: true }));
        return 'OK';
      })()`,
      returnByValue: true,
    });
    console.log("firewall app select:", appSelect.result?.value);
    await sleep(1000);
    await shot("permissions-firewall.png");
    console.log("tab autostart:", await click("自启动"));
    await sleep(900);
    await shot("permissions-autostart.png");
    console.log("tab capability:", await click("Store 能力"));
    await sleep(900);
    const capSelect = await cdp.send("Runtime.evaluate", {
      expression: `(() => {
        const sels = [...document.querySelectorAll('select')];
        const s = sels.find((x) => [...x.options].some((o) => (o.textContent || '').includes('Spotify')));
        if (!s) return 'NO_SELECT';
        const opt = [...s.options].find((o) => (o.textContent || '').includes('Spotify'));
        s.value = opt.value;
        s.dispatchEvent(new Event('change', { bubbles: true }));
        return 'OK';
      })()`,
      returnByValue: true,
    });
    console.log("capability select:", capSelect.result?.value);
    await sleep(1000);
    await shot("permissions-capability.png");

    console.log("done");
  } finally {
    try {
      cdp?.ws.close();
    } catch {
      /* ignore */
    }
    try {
      edge.kill();
      if (process.platform === "win32" && edge.pid) {
        spawnSync("taskkill", ["/PID", String(edge.pid), "/T", "/F"], { stdio: "ignore" });
      }
    } catch {
      /* ignore */
    }
  }
}

main().catch((e) => {
  console.error("FAILED:", e?.message ?? e);
  process.exitCode = 1;
});
