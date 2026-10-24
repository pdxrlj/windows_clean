/**
 * Dev-only: evaluate an expression in the preview page and print the result.
 * Usage: node tools/preview/probe.mjs "<url>" "<js expression>" [out.png] [w] [h]
 */
import { spawn, spawnSync } from "node:child_process";
import { writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";

const [url, expr, outPng, w = "1280", h = "920"] = process.argv.slice(2);
const EDGE = "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe";
const PORT = 9333;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class CDP {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(typeof ev.data === "string" ? ev.data : ev.data.toString());
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

const edge = spawn(
  EDGE,
  [
    "--headless=new",
    "--disable-gpu",
    "--no-first-run",
    `--remote-debugging-port=${PORT}`,
    `--user-data-dir=${path.join(os.tmpdir(), "cleansweep-probe-edge")}`,
    "--window-size=1280,920",
    "about:blank",
  ],
  { stdio: "ignore" }
);

try {
  for (;;) {
    try {
      if ((await fetch(`http://127.0.0.1:${PORT}/json/version`)).ok) break;
    } catch {
      /* retry */
    }
    await sleep(200);
  }
  const page = await (
    await fetch(`http://127.0.0.1:${PORT}/json/new?${encodeURIComponent(url)}`, { method: "PUT" })
  ).json();
  const cdp = new CDP(new WebSocket(page.webSocketDebuggerUrl));
  await new Promise((res) => cdp.ws.addEventListener("open", res, { once: true }));
  await cdp.send("Runtime.enable");
  await cdp.send("Emulation.setDeviceMetricsOverride", {
    width: Number(w),
    height: Number(h),
    deviceScaleFactor: 1,
    mobile: false,
  });
  await sleep(2500);
  const r = await cdp.send("Runtime.evaluate", {
    expression: expr,
    returnByValue: true,
    awaitPromise: true,
  });
  console.log(JSON.stringify(r.result?.value ?? r.result, null, 2));
  if (outPng) {
    await sleep(700);
    const { data } = await cdp.send("Page.captureScreenshot", { format: "png" });
    await writeFile(outPng, Buffer.from(data, "base64"));
    console.log("saved", outPng);
  }
  cdp.ws.close();
} finally {
  edge.kill();
  if (process.platform === "win32" && edge.pid) {
    spawnSync("taskkill", ["/PID", String(edge.pid), "/T", "/F"], { stdio: "ignore" });
  }
}
