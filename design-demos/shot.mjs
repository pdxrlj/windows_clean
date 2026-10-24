/**
 * 用 headless Edge 截本地 HTML 的图（不依赖 playwright）。
 * 用法：node design-demos/shot.mjs <in.html> <out.png> [w] [h]
 */
import { spawn } from "node:child_process";
import { writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import os from "node:os";

const [input, output, w = "1280", h = "800"] = process.argv.slice(2);
if (!input || !output) {
  console.error("usage: node shot.mjs <in.html>[#hash] <out.png> [w] [h]");
  process.exit(1);
}
// 允许用 #hash 指定页面内的状态（比如 dir-2.html#empty 展开未选中态）
const hashAt = input.indexOf("#");
const inputPath = hashAt === -1 ? input : input.slice(0, hashAt);
const hash = hashAt === -1 ? "" : input.slice(hashAt);
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9444;
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

const main = async () => {
  if (!existsSync(EDGE)) throw new Error(`Edge not found: ${EDGE}`);
  const edge = spawn(
    EDGE,
    [
      "--headless=new",
      "--disable-gpu",
      "--hide-scrollbars",
      "--no-first-run",
      "--force-device-scale-factor=1",
      `--remote-debugging-port=${PORT}`,
      `--user-data-dir=${path.join(os.tmpdir(), "huashu-shot-edge")}`,
      `--window-size=${w},${h}`,
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
      if (Date.now() - t0 > 20000) throw new Error("devtools timeout");
      await sleep(250);
    }
    const url = pathToFileURL(path.resolve(inputPath)).href + hash;
    const page = await (
      await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(url)}`, { method: "PUT" })
    ).json();
    const cdp = new CDP(new WebSocket(page.webSocketDebuggerUrl));
    await new Promise((resolve, reject) => {
      cdp.ws.addEventListener("open", resolve, { once: true });
      cdp.ws.addEventListener("error", reject, { once: true });
    });
    await cdp.send("Page.enable");
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: Number(w),
      height: Number(h),
      deviceScaleFactor: 1,
      mobile: false,
    });
    await sleep(1200);
    const { data } = await cdp.send("Page.captureScreenshot", { format: "png" });
    await writeFile(path.resolve(output), Buffer.from(data, "base64"));
    console.log("saved", output);
    cdp.ws.close();
  } finally {
    edge.kill();
  }
};

main().catch((e) => {
  console.error(e.message);
  process.exit(1);
});
