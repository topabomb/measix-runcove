"use strict";
function desktopCapture() {
  const cp = require("node:child_process");
  const front = () => {
    try {
      return cp
        .execFileSync("xdotool", ["getactivewindow"], {
          encoding: "utf8",
          timeout: 1500,
          maxBuffer: 65536,
        })
        .trim();
    } catch {
      return null;
    }
  };
  let window = front();
  const at = Date.now();
  const bytes = cp.execFileSync(
    "/opt/agent-tools/python/bin/python",
    [
      "-c",
      "import mss,mss.tools,sys; s=mss.mss(); im=s.grab(s.monitors[0]); sys.stdout.buffer.write(mss.tools.to_png(im.rgb,im.size))",
    ],
    { timeout: 4000, maxBuffer: 10 * 1024 * 1024 },
  );
  if (front() !== window) window = null;
  return {
    image_id: require("node:crypto").randomUUID(),
    width: bytes.readUInt32BE(16),
    height: bytes.readUInt32BE(20),
    window,
    at,
    source: "desktop",
    bytes,
  };
}

// One page-image contract for snapshot annotations and plain screen sequences.
// send is either a task-local Playwright CDP session or the passive raw CDP client.
async function pageCapture(send, { marks = [] } = {}) {
  const crypto = require("node:crypto");
  const expression =
    "(()=>({width:innerWidth,height:innerHeight,sx:scrollX,sy:scrollY,dpr:devicePixelRatio,url:location.href,document:document[Symbol.for('runcove.browser.document.v1')]||=(" +
    JSON.stringify(crypto.randomUUID()) +
    ")}))()";
  const state = async () => {
    const r = await send("Runtime.evaluate", {
      expression,
      returnByValue: true,
    });
    if (!r.result?.value)
      throw Error("PAGE_UNRESPONSIVE: Cannot read page geometry");
    return r.result.value;
  };
  const before = await state(),
    at = Date.now();
  const content = async () => {
    const r = await send("Page.getLayoutMetrics");
    return {
      width: Math.ceil(Math.max(r.cssContentSize.width, before.width)),
      height: Math.ceil(Math.max(r.cssContentSize.height, before.height)),
    };
  };
  const { width, height } = await content();
  // Preserve the original PNG. The shared Rust media pipeline performs the
  // single presentation resize; saved originals never contain a preview.
  if (
    !Number.isFinite(width * height * before.dpr ** 2) ||
    width * height * before.dpr ** 2 > 16_000_000 ||
    Math.max(width, height) * before.dpr > 32760
  )
    throw Error(
      "IMAGE_TOO_LARGE: Original page exceeds the 16 million pixel capture limit",
    );
  const overlay = "__runcove_" + crypto.randomUUID();
  let response;
  try {
    if (marks.length) {
      const args = JSON.stringify([overlay, marks, width, height]);
      const expression = `(([id,boxes,width,height])=>{const root=document.createElement('div');root.id=id;root.setAttribute('aria-hidden','true');root.style.cssText='position:absolute;left:0;top:0;width:'+width+'px;height:'+height+'px;pointer-events:none;z-index:2147483647;overflow:hidden;contain:strict';for(const m of boxes){const b=document.createElement('div');b.style.cssText='position:absolute;left:'+m.x+'px;top:'+m.y+'px;width:'+m.w+'px;height:'+m.h+'px;outline:2px solid #d23';const l=document.createElement('span');l.textContent=m.ref;l.style.cssText='background:#d23;color:white;font:11px monospace';b.append(l);root.append(b);}document.documentElement.append(root);})(${args})`;
      await send("Runtime.evaluate", { expression });
    }
    response = await send("Page.captureScreenshot", {
      format: "png",
      captureBeyondViewport: true,
      clip: { x: 0, y: 0, width, height, scale: 1 },
    });
  } finally {
    if (marks.length)
      await send("Runtime.evaluate", {
        expression:
          "document.getElementById(" + JSON.stringify(overlay) + ")?.remove()",
      }).catch(() => {});
  }
  const after = await state(),
    afterContent = await content();
  if (
    JSON.stringify(before) !== JSON.stringify(after) ||
    afterContent.width !== width ||
    afterContent.height !== height
  )
    throw Error(
      "STALE_OBSERVATION: Document changed during capture; observe again",
    );
  const bytes = Buffer.from(response.data, "base64");
  if (bytes.length > 10 * 1024 * 1024)
    throw Error("IMAGE_TOO_LARGE: Encoded page image exceeds 10 MiB");
  const pixelWidth = bytes.readUInt32BE(16),
    pixelHeight = bytes.readUInt32BE(20);
  return {
    image_id: crypto.randomUUID(),
    source: "page",
    at,
    document: before.document,
    url: before.url,
    sx: before.sx,
    sy: before.sy,
    viewport_width: before.width,
    viewport_height: before.height,
    content_width: width,
    content_height: height,
    full_page: true,
    width: pixelWidth,
    height: pixelHeight,
    scale: pixelWidth / width,
    bytes,
  };
}
const clipText = (value, limit) =>
  Array.from(String(value).toWellFormed()).slice(0, limit).join("");
module.exports = { desktopCapture, pageCapture, clipText };
