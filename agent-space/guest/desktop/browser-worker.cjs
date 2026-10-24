const fs = require("node:fs"),
  cp = require("node:child_process"),
  crypto = require("node:crypto");
const { chromium } = require("/opt/agent-tools/node_modules/playwright");
const tools = require("./browser-tools.cjs");
const timing = require("./timing.cjs");
const {
  publish,
  directory,
} = require("/opt/agent-space/desktop/artifacts.cjs");
const task = JSON.parse(process.env.AGENT_TASK);
delete process.env.AGENT_TASK;
const taskDeadline = performance.now() + (task.budget_ms ?? 120000);
let phaseDeadline = taskDeadline;
const remaining = () => Math.max(1, phaseDeadline - performance.now());
function armDeadline(ms, phase) {
  const previous = phaseDeadline;
  phaseDeadline = Math.min(taskDeadline, performance.now() + ms);
  const timer = Number.isFinite(ms) ? setTimeout(() => {
    // Terminate this per-request worker: a timed-out async action must never
    // resume in the background and execute later steps.
    if (phase.observation) {
      const error = { code: "PAGE_UNRESPONSIVE", message: "Page observation timed out." };
      const extra = { value: { ...saved.value, observation_error: error } };
      if (phase.failure) {
        const { step_number, do: action, ref, ...failure } = phase.failure;
        Object.assign(extra, { error: failure, failed_step: { step_number, do: action, ...(ref ? { ref } : {}) } });
      } else if (task.operation !== "act") extra.error = error;
      checkpoint(extra);
      process.exit(extra.error ? 1 : 0);
    }
    checkpoint({ error: { code: "TIMEOUT", message: "Action execution timed out." }, failed_step: phase,
      side_effects: saved.side_effects || "not_started" });
    process.exit(1);
  }, remaining()) : null;
  return () => { clearTimeout(timer); phaseDeadline = previous; };
}
const captures = new Map((task.captures || []).map((c) => [c.image_id, c]));
const images = [],
  files = [],
  emittedCaptures = [];
let bytes = 0;
// Atomic checkpoints retain explicitly registered output when later code fails
// or the execution owner terminates the worker.
let saved = {};
function checkpoint(extra = {}) {
  const result = Object.assign(saved, extra);
  if (images.length) result.images = images;
  if (files.length) result.files = files;
  if (emittedCaptures.length) result.captures = emittedCaptures;
  const path = process.env.AGENT_RESULT;
  fs.writeFileSync(
    path + ".tmp",
    JSON.stringify(result, (_key, value) =>
      typeof value === "string" ? value.toWellFormed() : value,
    ),
  );
  fs.renameSync(path + ".tmp", path);
}
const cmd = (name, args) =>
  cp.execFileSync(name, args, { encoding: "utf8", timeout: 4000 }).trim();
const front = () => cmd("xdotool", ["getactivewindow"]);
function active() {
  if (task.window && front() !== task.window)
    throw Error(
      "STALE_OBSERVATION: foreground window changed; observe desktop again",
    );
}
function coordinate(c) {
  if (
    !c ||
    typeof c !== "object" ||
    typeof c.image_id !== "string" ||
    !Number.isFinite(c.x) ||
    !Number.isFinite(c.y)
  )
    throw Error(
      "INVALID_ARGUMENT: use an image_id and coordinates from screen_observe",
    );
  const s = captures.get(c.image_id);
  if (!s || !s.window || Date.now() - s.at > 60000 || s.window !== front())
    throw Error(
      "STALE_OBSERVATION: capture expired or foreground window changed; observe desktop again",
    );
  if (c.x < 0 || c.y < 0 || c.x >= s.width || c.y >= s.height)
    throw Error(
      "INVALID_ARGUMENT: coordinates must be inside the returned screenshot",
    );
  const width = s.original_width || s.width,
    height = s.original_height || s.height;
  return [
    String(Math.min(width - 1, Math.round((c.x * width) / s.width))),
    String(Math.min(height - 1, Math.round((c.y * height) / s.height))),
  ];
}
const desktop = {
  validate: coordinate,
  click: async (c) => {
    const xy = coordinate(c);
    cmd("xdotool", [
      "mousemove",
      ...xy,
      "click",
      "--repeat",
      c.double ? "2" : "1",
      "--delay",
      "100",
      String(c.button || 1),
    ]);
  },
  drag: async (c) => {
    const xy = coordinate(c),
      end = coordinate({ ...c, x: c.to_x, y: c.to_y });
    cmd("xdotool", ["mousemove", ...xy, "mousedown", "1"]);
    try {
      cmd("xdotool", ["mousemove", ...end]);
    } finally {
      cmd("xdotool", ["mouseup", "1"]);
    }
  },
  scroll: async (c) => {
    const xy = coordinate(c),
      delta = c.delta;
    if (!Number.isInteger(delta) || Math.abs(delta) > 30)
      throw Error("INVALID_ARGUMENT: scroll delta must be -30..30");
    if (delta)
      cmd("xdotool", [
        "mousemove",
        ...xy,
        "click",
        "--repeat",
        String(Math.abs(delta)),
        delta > 0 ? "5" : "4",
      ]);
  },
  type: async (text) => {
    active();
    if (typeof text !== "string" || text.length > 65536)
      throw Error("invalid text");
    const clip = cp.spawn("xclip", ["-selection", "clipboard", "-quiet"], {
      stdio: ["pipe", "ignore", "ignore"],
    });
    clip.stdin.end(text);
    try {
      await new Promise((r) => setTimeout(r, 100));
      cmd("xdotool", ["key", "--clearmodifiers", "ctrl+v"]);
      await new Promise((r) => setTimeout(r, 200));
    } finally {
      clip.kill("SIGTERM");
    }
  },
  press: async (key) => {
    active();
    if (typeof key !== "string" || !/^[A-Za-z0-9_+ -]{1,80}$/.test(key))
      throw Error("invalid key");
    cmd("xdotool", [
      "key",
      "--clearmodifiers",
      key.split("+").map(part => ({
        Control: "ctrl", Alt: "alt", Shift: "shift", Meta: "super",
        Enter: "Return", Space: "space", Backspace: "BackSpace",
        ArrowLeft: "Left", ArrowRight: "Right", ArrowUp: "Up", ArrowDown: "Down",
        PageUp: "Prior", PageDown: "Next",
      })[part] || part).join("+"),
    ]);
  },
};
const output = {
  file: async (file) => {
    if (files.length >= 16) throw Error("At most 16 artifact files per task");
    const item = publish(file);
    files.push(item);
    checkpoint();
    return item;
  },
  download: async (download) => {
    const name = download
      .suggestedFilename()
      .replace(/[^\p{L}\p{N}._-]/gu, "_");
    const temp = directory + "/" + crypto.randomUUID() + "-" + name;
    await download.saveAs(temp);
    return await output.file(temp);
  },
  image: async (capture) => {
    const b = Buffer.isBuffer(capture) ? capture : capture?.bytes;
    if (!Buffer.isBuffer(b))
      throw Error(
        "INVALID_ARGUMENT: output.image requires a PNG, JPEG, or WebP Buffer, or a desktop Capture",
      );
    const mime = b
      .subarray(0, 8)
      .equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))
      ? "image/png"
      : b[0] === 255 && b[1] === 216
        ? "image/jpeg"
        : b.toString("ascii", 0, 4) === "RIFF" &&
            b.toString("ascii", 8, 12) === "WEBP"
          ? "image/webp"
          : null;
    if (!mime)
      throw Error(
        "INVALID_ARGUMENT: output.image supports PNG, JPEG, and WebP",
      );
    if (images.length >= 1 || bytes + b.length > 10 * 1024 * 1024)
      throw Error("capture output exceeds 1 original image / 10 MiB");
    bytes += b.length;
    images.push({ type: "image", mimeType: mime, data: b.toString("base64") });
    if (capture.image_id) {
      const { bytes, ...metadata } = capture;
      emittedCaptures.push(metadata);
    }
    checkpoint();
  },
};

const get = async (path) => {
  const r = await fetch("http://127.0.0.1:9222" + path, {
    signal: AbortSignal.timeout(2000),
  });
  if (!r.ok) throw Error("CDP_HTTP_" + r.status);
  return r.json();
};
async function cdp(url, method, params = {}) {
  return new Promise((resolve, reject) => {
    const w = new WebSocket(url),
      timer = setTimeout(
        () => finish(Error("PAGE_UNRESPONSIVE: CDP command timed out")),
        3000,
      );
    let ended = false;
    function finish(e, v) {
      if (ended) return;
      ended = true;
      clearTimeout(timer);
      w.close();
      e ? reject(e) : resolve(v);
    }
    w.onopen = () => w.send(JSON.stringify({ id: 1, method, params }));
    w.onmessage = (e) => {
      let m;
      try {
        m = JSON.parse(e.data);
      } catch {
        return;
      }
      if (m.id === 1) finish(m.error ? Error(m.error.message) : null, m.result);
    };
    w.onerror = () => finish(Error("BROWSER_DISCONNECTED: CDP disconnected"));
    w.onclose = () => finish(Error("BROWSER_DISCONNECTED: CDP disconnected"));
  });
}
const observe = async (page_id, view) =>
  JSON.parse(
    await new Promise((resolve, reject) =>
      cp.execFile(
        process.execPath,
        [__dirname + "/observe-worker.cjs", JSON.stringify({ page_id, view })],
        { timeout: Math.ceil(Math.min(timing.OBSERVATION_MS, remaining())), maxBuffer: 2 * 1024 * 1024 },
        (e, stdout, stderr) => {
          if (!e) return resolve(stdout);
          const timedOut = e.killed || /timeout|timed out/i.test(stderr || e.message);
          const detail = timedOut
            ? "Browser observation timed out. Inspect the desktop or observe again before continuing."
            : "Browser observation failed: " + String(stderr || e.message).trim().slice(0, 500);
          reject(Object.assign(Error(detail), {
            code: timedOut ? "PAGE_UNRESPONSIVE" : "BROWSER_UNAVAILABLE",
            detail,
          }));
        },
      ),
    ),
  );
let browser = null;
const environment = {
  output,
  desktop,
  checkpoint,
  armDeadline,
  remaining,
  capture: (id) => captures.get(id),
  observe,
  page: async (id) => {
    const state = await observe(id);
    if (state.error) throw Error(state.error.code + ": " + state.error.message);
    if (state.profilePicker)
      throw Error(
        "PROFILE_SELECTION_REQUIRED: Select a Chrome profile using desktop actions.",
      );
    if (state.nativeWindow)
      throw Error(
        "NATIVE_WINDOW: Use a desktop image and desktop actions until the native window closes.",
      );
    if (!state.page_id)
      throw Error("PAGE_NOT_FOUND: Open a tab with a tab new action.");
    if (state.health?.page === false)
      throw Error(
        "PAGE_UNRESPONSIVE: The page is blocked or unresponsive. Inspect screen_observe view=desktop for a JS/native dialog and use desktop actions; explicitly close a stalled tab when needed.",
      );
    if (!browser)
      browser = await chromium
        .connectOverCDP("http://127.0.0.1:9222", {
          // Cold session restoration initializes every existing page; observed
          // four-page VM restores need about 8 s. Attach once within the task's
          // existing deadline and cancellation owner.
          timeout: 15000,
          noDefaults: task.operation !== "act",
        })
        .catch((e) => {
          throw Error(
            "PAGE_UNRESPONSIVE: Browser attachment failed; inspect/close the unresponsive tab. " +
              e.message.split("\n")[0],
          );
        });
    for (const context of browser.contexts()) {
      // Passive tools leave dialogs opened during observation for the human or
      // a later explicit dialog action, rather than Playwright's auto-dismiss.
      if (task.operation !== "act") context.on("dialog", () => {});
      context.setDefaultTimeout(timing.INTERACTION_MS);
      context.setDefaultNavigationTimeout(timing.EXTENDED_MS);
      for (const p of context.pages()) {
        let s;
        try {
          s = await bounded(context.newCDPSession(p), 2000);
          const info = await bounded(s.send("Target.getTargetInfo"), 2000);
          if (info.targetInfo.targetId === state.page_id) return p;
        } finally {
          if (s) await bounded(s.detach(), 1000).catch(() => {});
        }
      }
    }
    throw Error("PAGE_NOT_FOUND: Selected tab no longer exists.");
  },
  newPage: async () => {
    const r = await fetch("http://127.0.0.1:" + task.guard_port + "/page", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ task_id: task.task_id }),
      signal: AbortSignal.timeout(6000),
    });
    const b = await r.json();
    if (!r.ok) throw Error(b.error || "BROWSER_UNAVAILABLE");
    return b.page_id;
  },
  tab: async (id, action, starting) => {
    const target = (await get("/json/list")).find(
      (t) => t.type === "page" && t.id === id,
    );
    if (!target) throw Error("PAGE_NOT_FOUND: Tab no longer exists.");
    const version = await get("/json/version");
    starting();
    let commandError;
    try {
      await cdp(
        version.webSocketDebuggerUrl,
        action === "close" ? "Target.closeTarget" : "Target.activateTarget",
        { targetId: id },
      );
    } catch (error) {
      if (action !== "close") throw error;
      commandError = error;
    }
    if (action === "close") {
      // Closing the last window can terminate Chromium before the CDP reply.
      // Confirm the target is gone; never replay a potentially completed close.
      const until = Date.now() + 3000;
      do {
        try {
          if (!(await get("/json/list")).some((t) => t.id === id)) return;
        } catch (error) {
          if (error.cause?.code === "ECONNREFUSED") return;
          commandError ||= error;
        }
        await new Promise((resolve) => setTimeout(resolve, 50));
      } while (Date.now() < until);
      throw (
        commandError || Error("PAGE_UNRESPONSIVE: Tab closure was not confirmed")
      );
    }
    if (action === "focus") task.window = front();
  },
  window: async (mode, id) => {
    const version = await get("/json/version");
    const { windowId, bounds } = await cdp(
      version.webSocketDebuggerUrl,
      "Browser.getWindowForTarget",
      { targetId: id },
    );
    if (bounds.windowState !== "normal")
      await cdp(version.webSocketDebuggerUrl, "Browser.setWindowBounds", {
        windowId,
        bounds: { windowState: "normal" },
      });
    await cdp(version.webSocketDebuggerUrl, "Browser.setWindowBounds", {
      windowId,
      bounds: { windowState: mode },
    });
  },
};
function bounded(p, ms) {
  let timer;
  return Promise.race([
    p,
    new Promise((_, reject) => {
      timer = setTimeout(
        () => reject(Error("PAGE_UNRESPONSIVE: Browser command timed out")),
        ms,
      );
    }),
  ]).finally(() => clearTimeout(timer));
}
(async () => {
  const result = await tools.run(task, environment);
  checkpoint(result);
  if (result.error) {
    console.error(result.error.code + ": " + result.error.message);
    process.exitCode = 1;
  }
  process.stdout.write("", () => process.exit(result.error ? 1 : 0));
})().catch((error) => {
  const e = tools.classify(error);
  checkpoint({
    error: e,
    ...(task.operation === "act"
      ? { side_effects: saved.side_effects || "not_started" }
      : {}),
  });
  console.error(e.code + ": " + e.message);
  process.exit(1);
});
