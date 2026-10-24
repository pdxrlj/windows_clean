/**
 * 抓参考站点的真实截图（CDP + headless Edge），供设计前"亲眼看"。
 * 用法：node design-demos/refs/grab.mjs
 */
import { spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import os from "node:os";
import path from "node:path";

const PORT = 9333;
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const OUT = path.resolve("design-demos/refs");
const TARGETS = [
  { name: "zed", url: "https://zed.dev", w: 1600, h: 1000 },
];

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
        msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
}

async function main() {
  if (!existsSync(EDGE)) throw new Error(`Edge not found: ${EDGE}`);
  await mkdir(OUT, { recursive: true });

  const edge = spawn(
    EDGE,
    [
      "--headless=new",
      "--disable-gpu",
      "--hide-scrollbars",
      "--no-first-run",
      "--no-default-browser-check",
      `--remote-debugging-port=${PORT}`,
      `--user-data-dir=${path.join(os.tmpdir(), "huashu-ref-edge")}`,
      "--window-size=1600,1000",
      "about:blank",
    ],
    { stdio: "ignore" },
  );

  try {
    const t0 = Date.now();
    for (;;) {
      try {
        const r = await fetch(`http://127.0.0.1:${PORT}/json/version`);
        if (r.ok) break;
      } catch {
        /* retry */
      }
      if (Date.now() - t0 > 20000) throw new Error("edge devtools timeout");
      await sleep(300);
    }

    for (const t of TARGETS) {
      const res = await fetch(
        `http://127.0.0.1:${PORT}/json/new?${encodeURIComponent("about:blank")}`,
        { method: "PUT" },
      );
      const page = await res.json();
      const cdp = new CDP(new WebSocket(page.webSocketDebuggerUrl));
      await new Promise((resolve, reject) => {
        cdp.ws.addEventListener("open", resolve, { once: true });
        cdp.ws.addEventListener("error", reject, { once: true });
      });
      await cdp.send("Page.enable");
      await cdp.send("Emulation.setDeviceMetricsOverride", {
        width: t.w,
        height: t.h,
        deviceScaleFactor: 1,
        mobile: false,
      });
      try {
        await cdp.send("Page.navigate", { url: t.url });
      } catch (e) {
        console.log(t.name, "navigate failed:", e.message);
      }
      await sleep(7000);
      try {
        const title = await cdp.send("Runtime.evaluate", {
          expression: "document.title + ' | ' + location.href",
          returnByValue: true,
        });
        console.log(t.name, "->", title.result?.value);
        const { data } = await cdp.send("Page.captureScreenshot", { format: "png" });
        await writeFile(path.join(OUT, `${t.name}.png`), Buffer.from(data, "base64"));
        console.log("saved", t.name);
      } catch (e) {
        console.log(t.name, "shot failed:", e.message);
      }
      cdp.ws.close();
    }
  } finally {
    edge.kill();
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
