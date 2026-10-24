"use strict";
// Fixed browser operations. Each request uses a fresh worker/Playwright connection.
// Opaque refs live on the actual node, never on a role/name/position signature.
const fs = require("node:fs"),
  path = require("node:path"),
  crypto = require("node:crypto");
const { pageCapture, clipText } = require("./observation.cjs");
const semantic = require("./semantic-observation.cjs");
const PIN = "runcove.browser.ref.v1",
  DOC = "runcove.browser.document.v1";
const INTERACTIVE = new Set([
  "button",
  "link",
  "textbox",
  "searchbox",
  "checkbox",
  "radio",
  "combobox",
  "listbox",
  "option",
  "menuitem",
  "menuitemcheckbox",
  "menuitemradio",
  "tab",
  "switch",
  "slider",
  "spinbutton",
  "treeitem",
]);
const CONTEXT = new Set([
  "heading",
  "dialog",
  "alertdialog",
  "alert",
  "status",
  "tooltip",
  "menu",
  "tree",
  "region",
  "tabpanel",
  "iframe",
]);
const TEXTUAL = new Set([
  "paragraph",
  "text",
  "cell",
  "gridcell",
  "listitem",
  "generic",
  "strong",
  "emphasis",
  "blockquote",
  "caption",
  "term",
  "definition",
  "code",
]);
const MAX_NODES = 600;
const REF_RE = /^e[A-Za-z0-9_-]{10}$/;
const STEP_FIELDS = {
  click: ["ref", "image_id", "x", "y", "button", "double"],
  fill: ["ref", "text", "submit"], select: ["ref", "value"], check: ["ref", "checked"],
  press: ["target", "key", "ref"], hover: ["ref"],
  drag: ["ref", "to_ref", "image_id", "x", "y", "to_x", "to_y"],
  upload: ["ref", "paths"], download: ["ref", "timeout_secs"],
  scroll: ["target", "image_id", "x", "y", "delta", "ref", "until_text", "dy", "exact", "max"],
  wait: ["text", "gone", "secs", "timeout_secs"], navigate: ["mode", "url"],
  tab: ["action", "page_id", "url"], dialog: ["accept", "text"],
  desktop_type: ["text"], window: ["mode"],
};
const REQUIRED = {
  fill: ["ref", "text"], select: ["ref", "value"], check: ["ref"], press: ["key"],
  hover: ["ref"], upload: ["ref", "paths"], download: ["ref"],
  tab: ["action"], dialog: ["accept"], desktop_type: ["text"], window: ["mode"],
};
function validate(args) {
  if (!Array.isArray(args.actions) || args.actions.length < 1 || args.actions.length > 20)
    throw fault("INVALID_ARGUMENT", "actions must contain 1-20 steps; nothing was executed.");
  for (const [i, a] of args.actions.entries()) {
    const fields = a && STEP_FIELDS[a.do];
    let good = !!fields && typeof a === "object" && !Array.isArray(a) &&
      Object.keys(a).every(k => k === "do" || fields.includes(k));
    if (good) {
      let ignored = [];
      if (["click", "drag"].includes(a.do) && a.ref != null)
        ignored = ["image_id", "x", "y", ...(a.do === "drag" ? ["to_x", "to_y"] : [])];
      if (a.do === "press" && a.target === "desktop") ignored = ["ref"];
      if (a.do === "scroll") {
        ignored = a.target === "desktop" ? ["ref", "until_text", "dy", "exact", "max"] :
          ["image_id", "x", "y", "delta", ...(a.ref != null ? ["until_text", "dy", "exact", "max"] : a.until_text == null ? ["exact", "max"] : [])];
      }
      if (a.do === "wait" && a.secs != null) ignored = ["timeout_secs"];
      if (a.do === "tab") ignored = a.action === "new" ? ["page_id"] : ["url"];
      if (a.do === "navigate" && a.mode != null && a.mode !== "open") ignored = ["url"];
      for (const key of ignored) delete a[key];
      good = (REQUIRED[a.do] || []).every(k => a[k] != null);
      for (const [k,v] of Object.entries(a)) {
        if (v == null || k === "do") continue;
        if (["ref", "to_ref"].includes(k)) good &&= typeof v === "string" && REF_RE.test(v);
        else if (["x", "y", "to_x", "to_y"].includes(k)) good &&= Number.isFinite(v) && v >= 0;
        else if (["double", "submit", "checked", "accept", "exact"].includes(k)) good &&= typeof v === "boolean";
        else if (k === "paths") good &&= Array.isArray(v) && v.length > 0 && v.length <= 20 && v.every(p => typeof p === "string" && p.startsWith("/") && !p.includes("\0"));
        else if (["secs", "timeout_secs", "dy", "max", "delta"].includes(k)) good &&= Number.isInteger(v);
        else good &&= typeof v === "string";
      }
    }
    if (good && a.target != null) good = ["page", "desktop"].includes(a.target);
    if (good && a.button != null) good = ["left", "middle", "right"].includes(a.button);
    if (good && ["click", "drag"].includes(a.do)) {
      const required = a.ref != null ? (a.do === "drag" ? ["to_ref"] : []) :
        ["image_id", "x", "y", ...(a.do === "drag" ? ["to_x", "to_y"] : [])];
      good = required.every(k => a[k] != null) && !(a.ref == null && a.to_ref != null);
    }
    if (good && a.do === "wait") good = ["text", "gone", "secs"].filter(k => a[k] != null).length === 1 &&
      (a.secs == null || (a.secs >= 0 && a.secs <= 60)) &&
      (a.timeout_secs == null || (a.timeout_secs >= 1 && a.timeout_secs <= 60));
    if (good && a.do === "scroll") good = a.target === "desktop" ?
      ["image_id", "x", "y", "delta"].every(k => a[k] != null) && Math.abs(a.delta) <= 30 :
      (a.dy == null || Math.abs(a.dy) <= 20000) && (a.max == null || (a.max >= 1 && a.max <= 30));
    if (good && a.do === "tab") good = ["new", "close", "focus"].includes(a.action);
    if (good && a.do === "dialog") good = args.actions.slice(i+1).some(step => step &&
      !["dialog", "tab", "window", "desktop_type"].includes(step.do) &&
      !(["press", "scroll"].includes(step.do) && step.target === "desktop") && !(step.do === "wait" && step.secs != null));
    if (good && a.do === "navigate") good = ["open", "back", "forward", "reload"].includes(a.mode ?? "open") &&
      ((a.mode != null && a.mode !== "open") || typeof a.url === "string");
    if (good && a.do === "window") good = ["normal", "maximized", "fullscreen"].includes(a.mode);
    if (good && a.do === "download" && a.timeout_secs != null) good = a.timeout_secs >= 1 && a.timeout_secs <= 60;
    if (good && a.do === "press") good = a.target === "desktop" ? /^[A-Za-z0-9_+ -]{1,80}$/.test(a.key) : a.key.length > 0;
    if (good && a.do === "desktop_type") good = Buffer.byteLength(a.text) <= 65536;
    if (good && a.image_id != null) good = a.image_id.length > 0;
    if (good && a.page_id != null) good = a.page_id.trim().length > 0;
    if (good && ["text", "gone", "until_text"].some(k => a[k] != null))
      good = ["text", "gone", "until_text"].every(k => a[k] == null || a[k].length > 0 || ["fill", "desktop_type", "dialog"].includes(a.do));
    if (good && a.url != null) {
      try { good = ["http:", "https:", "file:", "about:", "data:"].includes(new URL(a.url).protocol); }
      catch { good = false; }
    }
    if (!good) throw fault("INVALID_ARGUMENT", "Invalid step " + i + "; nothing was executed.");
  }
}
const freshRef = () =>
  "e" + crypto.randomBytes(8).toString("base64url").slice(0, 10);
function framePath(node) {
  const frames = [];
  for (let p = node.parent; p; p = p.parent)
    if (p.role === "iframe") frames.unshift(p.ref || p.name || "iframe");
  return frames;
}
function fault(code, message) {
  return Object.assign(Error(code + ": " + message), { code, detail: message });
}
function bounded(p, ms, message = "page did not respond") {
  let timer;
  return Promise.race([
    p,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(fault("PAGE_UNRESPONSIVE", message)), ms);
    }),
  ]).finally(() => clearTimeout(timer));
}
function classify(e, page) {
  // Child-process failures use numeric exit codes; they are not tool codes.
  if (typeof e.code === "string" && /^[A-Z][A-Z0-9_]*$/.test(e.code))
    return {
      code: e.code,
      message: e.detail || String(e.message).slice(0, 1000),
    };
  const m = String(e.message || e),
    first = m.split("\n")[0];
  const named = /^([A-Z_]+): (.*)$/.exec(first);
  if (named) return { code: named[1], message: named[2].slice(0, 1000) };
  if (page?.__runcoveCrashed || /page crashed|target crashed/i.test(m))
    return {
      code: "PAGE_CRASHED",
      message:
        "The tab crashed; inspect the desktop, reload with press(target=desktop), or close it with tab action=close.",
    };
  if (/closed|disconnected/i.test(first))
    return {
      code:
        page?.context().browser()?.isConnected() === false
          ? "BROWSER_DISCONNECTED"
          : "PAGE_CLOSED",
      message: first.slice(0, 500),
    };
  if (/Frame was detached|frame got detached/i.test(m))
    return {
      code: "FRAME_DETACHED",
      message: "The iframe was removed or navigated; take a fresh snapshot.",
    };
  if (/Execution context was destroyed|net::ERR_ABORTED/i.test(m))
    return {
      code: "NAVIGATED",
      message:
        "The document changed during the action; observe its current state before continuing.",
    };
  const cover =
    /(<[^\n]{0,200}?>)(?: from [^\n]*?)? (?:subtree )?intercepts pointer events/.exec(
      m,
    );
  if (cover)
    return {
      code: "ELEMENT_COVERED",
      message:
        "Target is covered by " +
        cover[1] +
        "; inspect the current observation for the covering element.",
    };
  if (/Timeout|timed out/i.test(m))
    return {
      code: "TIMEOUT",
      message: first.slice(0, 500) + "; inspect current state before retrying.",
    };
  if (/not an? |not a checkbox|not editable|not an HTML/i.test(first))
    return { code: "INVALID_TARGET", message: first.slice(0, 500) };
  return { code: "ACTION_FAILED", message: first.slice(0, 1000) };
}
const LINE =
  /^(\s*)- ([a-zA-Z][\w-]*)(?: "((?:[^"\\]|\\.)*)")?((?: \[[^\]]*\])*)(?::\s?(.*))?$/;
function parse(raw) {
  const nodes = [],
    stack = [];
  for (let line of raw.split("\n")) {
    const quoted = /^(\s*)- '((?:[^']|'')*)'(:.*)?$/.exec(line);
    if (quoted)
      line =
        quoted[1] + "- " + quoted[2].replace(/''/g, "'") + (quoted[3] || "");
    const m = LINE.exec(line);
    if (!m) continue;
    const attrs = {};
    for (const a of (m[4] || "").match(/\[[^\]]*\]/g) || []) {
      const [k, ...v] = a.slice(1, -1).split("=");
      attrs[k] = v.length ? v.join("=") : true;
    }
    const depth = m[1].length / 2,
      role = m[2];
    while (stack.length && stack.at(-1).depth >= depth) stack.pop();
    const node = {
      depth,
      role,
      name: m[3] ? JSON.parse('"' + m[3] + '"') : "",
      text: m[5]?.trim(),
      attrs,
      pw: attrs.ref,
      parent: stack.at(-1) || null,
    };
    if (role === "text" && !node.name) node.name = node.text || "";
    nodes.push(node);
    stack.push(node);
  }
  return nodes;
}
async function documentId(page) {
  return bounded(
    page.evaluate(
      ([key, candidate]) => (document[Symbol.for(key)] ||= candidate),
      [DOC, crypto.randomUUID()],
    ),
    3000,
  );
}
async function view(page, boxes = false) {
  const nodes = parse(
    await bounded(
      page.ariaSnapshot({ mode: "ai", boxes, timeout: 10000 }),
      12000,
    ),
  );
  const candidates = nodes.filter(
    (n) =>
      n.pw &&
      (INTERACTIVE.has(n.role) ||
        CONTEXT.has(n.role) ||
        TEXTUAL.has(n.role) ||
        n.attrs.cursor === "pointer"),
  );
  const selected = candidates.slice(0, MAX_NODES),
    refs = new Map();
  // Bounded groups avoid flooding the connection on large pages.
  for (let i = 0; i < selected.length; i += 24)
    await Promise.all(
      selected.slice(i, i + 24).map(async (n) => {
        n.ref = await page
          .locator("aria-ref=" + n.pw)
          .evaluate(
            (el, [pin, doc, documentCandidate, refCandidate]) => {
              const owner = el.ownerDocument,
                generation = (owner[Symbol.for(doc)] ||= documentCandidate);
              let record = el[Symbol.for(pin)];
              if (!record || record.document !== generation)
                el[Symbol.for(pin)] = record = {
                  document: generation,
                  ref: refCandidate,
                };
              return record.ref;
            },
            [PIN, DOC, crypto.randomUUID(), freshRef()],
            { timeout: 1500 },
          )
          .catch(() => null);
        if (n.ref) refs.set(n.ref, n);
      }),
    );
  return { nodes, refs, truncated: candidates.length > MAX_NODES };
}
function subtree(v, ref) {
  const root = v.refs.get(ref);
  if (!root)
    throw fault(
      "STALE_REF",
      "This ref is no longer in the current document. Use fresh refs from an observation; do not repeat completed steps.",
    );
  const inside = (n) => {
    for (let p = n; p; p = p.parent) if (p === root) return true;
    return false;
  };
  return {
    ...v,
    nodes: v.nodes.filter(inside),
    refs: new Map([...v.refs].filter(([, n]) => inside(n))),
  };
}
async function layers(page) {
  const all = [];
  let incomplete = false;
  for (const frame of page.frames()) {
    const frameLayers = await bounded(
      frame.evaluate(
        ([pin, doc, interactive]) => {
          const visible = (e) => {
            const b = e.getBoundingClientRect(),
              s = getComputedStyle(e);
            return (
              b.width > 0 &&
              b.height > 0 &&
              s.visibility !== "hidden" &&
              s.display !== "none"
            );
          };
          const descendants = (root, includeRoot = false) => {
            const items = includeRoot ? [root] : [],
              roots = [root];
            for (const current of roots) {
              if (current === root && current.shadowRoot)
                roots.push(current.shadowRoot);
              for (const e of current.querySelectorAll("*")) {
                items.push(e);
                if (e.shadowRoot) roots.push(e.shadowRoot);
              }
            }
            return items;
          };
          const elements = descendants(document);
          const controlled = new Set(
            elements.flatMap((e) =>
              (e.getAttribute("aria-expanded") === "true" ? e.getAttribute("aria-controls") || "" : "")
                .split(/\s+/)
                .filter(Boolean),
            ),
          );
          const semantic = (e) =>
            ["dialog", "alertdialog", "menu", "listbox", "tooltip"].includes(
              e.getAttribute("role"),
            ) || e.matches('dialog[open],:popover-open,[aria-modal="true"]');
          const candidates = elements.filter(e => visible(e) && (semantic(e) || controlled.has(e.id)));
          return {
            truncated: candidates.length > 20,
            rows: candidates.slice(0, 20).map((e) => {
              const generation = (document[Symbol.for(doc)] ||= crypto.randomUUID());
              if (!e[Symbol.for(pin)] || e[Symbol.for(pin)].document !== generation) {
                const bytes = crypto.getRandomValues(new Uint8Array(8));
                e[Symbol.for(pin)] = {document: generation, ref: "e" + btoa(String.fromCharCode(...bytes)).replace(/\+/g,"-").replace(/\//g,"_").slice(0,10)};
              }
              const refs = descendants(e, true)
                .filter((n) => n === e || n.matches('button,input,textarea,select,a[href],[contenteditable="true"]') || interactive.includes(n.getAttribute("role")))
                .map((n) => n[Symbol.for(pin)])
                .filter((p) => p?.document === document[Symbol.for(doc)]);
              return {
                ref: e[Symbol.for(pin)].ref,
                role:
                  e.getAttribute("role") ||
                  (e.tagName === "DIALOG" ? "dialog" : "panel"),
                name:
                  e.getAttribute("aria-label") ||
                  (e.getAttribute("aria-labelledby") || "")
                    .split(/\s+/)
                    .map(
                      (id) =>
                        e.getRootNode().getElementById(id)?.textContent || "",
                    )
                    .join(" ")
                    .trim()
                    .slice(0, 200),
                modal:
                  e.getAttribute("aria-modal") === "true" ||
                  e.matches(":modal"),
                refs: refs.slice(0, 600).map(p => p.ref),
                ...(refs.length > 600 ? { truncated: true } : {}),
              };
            }),
          };
        },
        [PIN, DOC, [...INTERACTIVE]],
      ),
      3000,
    );
    incomplete ||= frameLayers.truncated;
    all.push(
      ...frameLayers.rows.map((r) => ({
        ...r,
        ...(frame !== page.mainFrame() ? {
          iframe: true, frame_url: clipText(frame.url(), 500),
        } : {}),
        ...(frameLayers.truncated ? { frame_truncated: true } : {}),
      })),
    );
    if (all.length > 40) break;
  }
  return {
    ...(all.length ? { layers: all.slice(0, 40) } : {}),
    ...(incomplete ||
      all.length > 40 ||
      all.some((r) => r.truncated || r.frame_truncated) ? { layers_truncated: true } : {}),
  };
}
async function capture(page, output, v) {
  const scroll = await bounded(
      page.evaluate(() => ({ x: scrollX, y: scrollY })),
      3000,
    ),
    cache = new Map();
  const box = async (n) => {
    if (!cache.has(n))
      cache.set(
        n,
        bounded(
          page.locator("aria-ref=" + n.pw).boundingBox({ timeout: 1500 }),
          2000,
        ).catch(() => null),
      );
    return cache.get(n);
  };
  const marks = [];
  for (const n of [...v.refs.values()]
    .filter((n) => INTERACTIVE.has(n.role))
    .slice(0, 150)) {
    const b = await box(n);
    if (!b) continue;
    let left = b.x,
      top = b.y,
      right = b.x + b.width,
      bottom = b.y + b.height;
    // Child controls are drawn only inside the iframe regions actually rendered.
    for (let parent = n.parent; parent; parent = parent.parent)
      if (parent.role === "iframe") {
        const frame = await box(parent);
        if (!frame) {
          right = left;
          break;
        }
        left = Math.max(left, frame.x);
        top = Math.max(top, frame.y);
        right = Math.min(right, frame.x + frame.width);
        bottom = Math.min(bottom, frame.y + frame.height);
      }
    if (right > left && bottom > top)
      marks.push({
        ref: n.ref,
        x: left + scroll.x,
        y: top + scroll.y,
        w: right - left,
        h: bottom - top,
      });
  }
  const session = await bounded(page.context().newCDPSession(page), 3000);
  try {
    const image = await pageCapture(
      (method, args) =>
        bounded(
          session.send(method, args),
          method === "Page.captureScreenshot" ? 10000 : 3000,
        ),
      { marks },
    );
    image.page_id = await pageId(page);
    await output.image(image);
    return {
      image_id: image.image_id,
      width: image.width,
      height: image.height,
      scale: image.scale,
      full_page: true,
      document_width: image.content_width,
      document_height: image.content_height,
    };
  } finally {
    await bounded(session.detach(), 1000).catch(() => {});
  }
}
async function pageId(page) {
  const s = await bounded(page.context().newCDPSession(page), 3000);
  try {
    return (await bounded(s.send("Target.getTargetInfo"), 3000)).targetInfo
      .targetId;
  } finally {
    await bounded(s.detach(), 1000).catch(() => {});
  }
}
async function resolve(page, ref) {
  const v = await view(page),
    n = v.refs.get(ref);
  if (!n)
    throw fault(
      "STALE_REF",
      "This element is no longer available. Observe the current page and use a fresh ref; do not repeat completed steps.",
    );
  return page.locator("aria-ref=" + n.pw);
}
async function waitText(page, text, appear, timeout) {
  const end = Date.now() + timeout;
  const query = text.toLowerCase();
  const literal = new RegExp(text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "i");
  do {
    if (
      page.isClosed() ||
      page.__runcoveCrashed ||
      !page.context().browser().isConnected()
    )
      throw fault("PAGE_CLOSED", "The page closed or crashed while waiting.");
    let visible = false, observed = false;
    try {
      // The same accessible names/text as snapshot and query, without pinning
      // refs or updating their baseline on every poll.
      const budget = () => Math.max(1, Math.min(1500, end - Date.now()));
      const nodes = parse(await bounded(
        page.ariaSnapshot({ mode: "ai", timeout: budget() }), budget(),
      ));
      visible = nodes.some((n) => matchesText(n, query));
      // Visible aria-hidden text is absent from the accessibility snapshot.
      if (!visible)
        for (const f of page.frames()) {
          if (await bounded(f.getByText(literal).filter({ visible: true }).count(), budget())) {
            visible = true;
            break;
          }
        }
      observed = true;
    } catch {
      // An unreadable frame/document is not evidence that text disappeared.
    }
    if (observed && visible === appear) return;
    await new Promise((r) => setTimeout(r, Math.min(100, Math.max(0, end - Date.now()))));
  } while (Date.now() < end);
  throw fault(
    "TIMEOUT",
    "Visible text or element name " +
      JSON.stringify(text) +
      (appear ? " did not appear" : " remained visible") +
      "; inspect the current observation before continuing.",
  );
}
async function settle(page) {
  if (!page || page.isClosed() || page.__runcoveCrashed) return;
  // Renderer timers cannot bound a frozen renderer: the outer timer is required.
  await bounded(
    page.evaluate(
      () =>
        new Promise((resolve) => {
          let quiet;
          const finish = () => {
            observer.disconnect();
            clearTimeout(quiet);
            resolve();
          };
          const observer = new MutationObserver(() => {
            clearTimeout(quiet);
            quiet = setTimeout(finish, 120);
          });
          observer.observe(document, {
            subtree: true,
            childList: true,
            attributes: true,
            characterData: true,
          });
          quiet = setTimeout(finish, 120);
          setTimeout(finish, 1000);
        }),
    ),
    1500,
  ).catch(() => {});
}
function uploadFiles(paths) {
  return paths.map((p) => {
    let real;
    try {
      real = fs.realpathSync(p);
    } catch {
      throw fault("FILE_NOT_FOUND", "Upload file does not exist: " + p);
    }
    fs.accessSync(real, fs.constants.R_OK);
    const s = fs.statSync(real);
    if (!s.isFile())
      throw fault("INVALID_ARGUMENT", "Upload path is not a regular file.");
    return { path: real, name: path.basename(real), bytes: s.size };
  });
}
function watch(context, output, policy, modifying) {
  const downloads = [],
    popups = [],
    dialogs = [],
    pending = new Map(),
    hooked = new Set();
  const save = (d) => {
    if (pending.has(d)) return pending.get(d);
    const p = (async () => {
      const failure = await d.failure();
      if (failure) throw fault("DOWNLOAD_FAILED", failure);
      return compactObservation({ ...(await output.download(d)), url: d.url() });
    })().catch((e) => compactObservation({
      error: String(e.message || e).slice(0, 1000),
      name: d.suggestedFilename(),
      url: d.url(),
    }));
    pending.set(d, p);
    downloads.push(p);
    return p;
  };
  const onDialog = (d) => {
    modifying();
    dialogs.push({
      type: d.type(),
      message: clipText(d.message(), 500),
      action: policy.accept ? "accepted" : "dismissed",
    });
    (policy.accept && d.type() !== "beforeunload"
      ? d.accept(policy.text)
      : d.dismiss()
    ).catch(() => {});
  };
  const onCrash = function () {
    this.__runcoveCrashed = true;
  };
  const hook = (p) => {
    if (hooked.has(p)) return;
    hooked.add(p);
    p.on("download", save);
    p.on("dialog", onDialog);
    p.on("crash", onCrash);
  };
  const onPage = (p) => {
    popups.push(p);
    hook(p);
  };
  for (const p of context.pages()) hook(p);
  context.on("page", onPage);
  return {
    save,
    async finish(result) {
      context.off("page", onPage);
      for (const p of hooked) {
        p.off("download", save);
        p.off("dialog", onDialog);
        p.off("crash", onCrash);
      }
      if (downloads.length)
        result.downloads = await Promise.all(
          downloads.slice(0, 16).map((p) =>
            bounded(p, 15000).catch(() => ({
              error: "Download still in progress when the task ended.",
            })),
          ),
        );
      if (dialogs.length) result.dialogs = dialogs.slice(0, 20);
      if (popups.length)
        result.popups = await Promise.all(
          popups.slice(0, 20).map(async (p) => compactObservation({
            page_id: await pageId(p).catch(() => null),
            url: p.url(),
          })),
        );
      return result;
    },
  };
}

function matchesText(node, query) {
  return (node.name + " " + (node.text || "")).toLowerCase().includes(query);
}
async function read(page, args) {
  let v;
  if (args.query || args.ref || args.extract === "fields") v = await view(page);
  if (args.ref) v = subtree(v, args.ref);
  if (args.query) {
    const query = args.query.toLowerCase();
    const matches = v.nodes
      .filter(
        (n) =>
          n.ref &&
          matchesText(n, query),
      )
      .map((n) => ({
        ref: n.ref,
        role: n.role,
        name: clipText(n.name, 120),
        text: n.text ? clipText(n.text, 300) : undefined,
        iframe: framePath(n).length > 0,
        frame_path: framePath(n),
        truncated:
          Array.from(n.name).length > 120 ||
          Array.from(n.text || "").length > 300,
      }));
    return {
      matches: matches.slice(0, 20),
      truncated:
        v.truncated ||
        matches.length > 20 ||
        matches.slice(0, 20).some((n) => n.truncated),
    };
  }
  if (args.extract === "fields") {
    const candidates = [...v.refs.values()].filter((n) =>
      [
        "textbox",
        "searchbox",
        "combobox",
        "checkbox",
        "radio",
        "switch",
        "spinbutton",
        "slider",
      ].includes(n.role),
    );
    const fields = [];
    for (let i = 0; i < Math.min(candidates.length, 150); i += 24)
      fields.push(
        ...(await Promise.all(
          candidates.slice(i, Math.min(i + 24, 150)).map(async (n) => {
            // Accessibility snapshots normalize whitespace. Structured field values
            // must come from the pinned control itself, including textarea newlines
            // and select option values rather than their displayed labels.
            const actual = await page.locator("aria-ref=" + n.pw).evaluate(
              (e) => {
                const raw =
                  typeof e.value === "string" || typeof e.value === "number"
                    ? String(e.value)
                    : (e.getAttribute("aria-valuetext") ??
                      e.getAttribute("aria-valuenow") ??
                      e.innerText ??
                      "");
                const chars = Array.from(raw);
                return {
                  value: chars.slice(0, 1000).join(""),
                  truncated: chars.length > 1000,
                };
              },
              undefined,
              { timeout: 1500 },
            );
            return {
              ref: n.ref,
              role: n.role,
              name: clipText(n.name, 120),
              value: actual.value,
              ...Object.fromEntries(
                ["checked", "disabled", "expanded", "selected", "required"]
                  .filter((k) => n.attrs[k] !== undefined)
                  .map((k) => [k, n.attrs[k]]),
              ),
              iframe: framePath(n).length > 0,
              frame_path: framePath(n),
              truncated: Array.from(n.name).length > 120 || actual.truncated,
            };
          }),
        )),
      );
    return {
      fields,
      truncated:
        v.truncated ||
        candidates.length > 150 ||
        fields.some((n) => n.truncated),
    };
  }
  const scopes = args.ref
    ? [page.locator("aria-ref=" + v.refs.get(args.ref).pw)]
    : page.frames().map((f) => f.locator("body"));
  const items = [];
  let truncated = false;
  for (const scope of scopes) {
    const result = await bounded(
      scope.evaluate((root, mode) => {
        const clip = (text, max) =>
          Array.from(String(text)).slice(0, max).join("");
        const visible = (e) => {
          const r = e.getBoundingClientRect();
          return (
            r.width > 0 &&
            r.height > 0 &&
            getComputedStyle(e).visibility !== "hidden"
          );
        };
        if (mode === "table") {
          const all = [
              ...(root.matches?.("table") ? [root] : []),
              ...root.querySelectorAll("table"),
            ].filter(visible),
            tables = all.slice(0, 5).map((t) => {
              const rows = [...t.rows]
                .slice(0, 102)
                .map((r) =>
                  [...r.cells]
                    .slice(0, 40)
                    .map((c) => clip(c.innerText.trim(), 300)),
                );
              const head =
                t.tHead?.rows.length ||
                [...(t.rows[0]?.cells || [])].every((c) => c.tagName === "TH")
                  ? rows.shift()
                  : null;
              return {
                headers: head,
                rows: rows.slice(0, 100),
                row_count: t.rows.length,
                truncated:
                  t.rows.length > (head ? 101 : 100) ||
                  [...t.rows]
                    .slice(0, 101)
                    .some(
                      (r) =>
                        r.cells.length > 40 ||
                        [...r.cells]
                          .slice(0, 40)
                          .some((c) => c.innerText.trim().length > 300),
                    ),
              };
            });
          return {
            items: tables,
            truncated: all.length > 5 || tables.some((t) => t.truncated),
          };
        }
        if (mode === "links") {
          const all = [
            ...(root.matches?.("a[href]") ? [root] : []),
            ...root.querySelectorAll("a[href]"),
          ].filter(visible);
          return {
            items: all.slice(0, 200).map((e) => ({
              text: clip(e.innerText.trim(), 200),
              href: clip(e.href, 2000),
            })),
            truncated:
              all.length > 200 ||
              all
                .slice(0, 200)
                .some(
                  (e) =>
                    e.innerText.trim().length > 200 || e.href.length > 2000,
                ),
          };
        }
        const main =
          root === document.body
            ? document.querySelector('main,[role="main"],article') || root
            : root;
        const text = ["INPUT", "TEXTAREA", "SELECT"].includes(main.tagName)
          ? String(main.value)
          : (main.innerText ?? main.textContent ?? "");
        return {
          items: [clip(text, 20001)],
          truncated: text.length > 20001,
        };
      }, args.extract || "text"),
      5000,
    ).catch((e) => {
      if (args.ref) throw e;
      return { items: [], truncated: true };
    });
    items.push(...result.items);
    truncated ||= result.truncated;
  }
  if (args.extract === "table")
    return {
      tables: items.slice(0, 5),
      truncated: truncated || items.length > 5,
    };
  if (args.extract === "links")
    return {
      links: items.slice(0, 200),
      truncated: truncated || items.length > 200,
    };
  const text = items.join("\n[iframe]\n").trim(),
    max = args.max_chars ?? 4000;
  return {
    text: clipText(text, max),
    chars: Array.from(text).length,
    truncated: truncated || Array.from(text).length > max,
  };
}

function compactObservation(value) {
  for (const [key, limit] of [["url", 1000], ["frame_url", 1000], ["title", 200]]) {
    if (typeof value[key] === "string" && Array.from(value[key]).length > limit) {
      value[key] = clipText(value[key], limit);
      value.truncated = true;
    }
  }
  if (!value.truncated) delete value.chars;
  for (const key of ["truncated", "iframe"])
    if (value[key] === false) delete value[key];
  for (const key of ["frame_path"])
    if (Array.isArray(value[key]) && value[key].length === 0) delete value[key];
  for (const key of ["matches", "fields", "tables"])
    for (const item of value[key] || []) compactObservation(item);

  return value;
}

async function run(task, env) {
  const args = task.args || {},
    policy = { accept: false },
    hooks = new Map();
  let current = task.page_id,
    page = null,
    done = 0,
    started = false,
    mutated = false;
  if (task.operation === "act") validate(args);
  if (task.operation === "snapshot" && args.ref && args.diff)
    throw fault("INVALID_ARGUMENT", "ref cannot be combined with diff=true.");
  let observeTab = false, failure = null;
  const begin = () => {
    started = true;
    env.checkpoint({
      value: { completed: done, of: args.actions.length, page_id: current },
      side_effects: "may_have_occurred",
    });
  };
  if (task.operation === "act")
    env.checkpoint({
      value: { completed: 0, of: args.actions.length, page_id: current },
      side_effects: "not_started",
    });
  const getPage = async () => {
    page = await env.page(current);
    current = await pageId(page);
    if (task.operation === "act" && !hooks.has(page.context()))
      hooks.set(
        page.context(),
        watch(page.context(), env.output, policy, begin),
      );
    return page;
  };
  const metadata = async () => {
    const m = await env.observe(current);
    const unhealthy = m.health && Object.entries(m.health).some(([key, value]) =>
      value === false && key !== "native_window");
    return {
      page_id: m.page_id,
      ...(unhealthy ? { health: m.health } : {}),
      ...(m.error ? { observe_error: m.error } : {}),
    };
  };
  const observe = async (mode, scope) => {
    const p = await getPage(), beforeDocument = await documentId(p),
      v = await view(p);
    const selected = scope ? subtree(v, scope) : v;
    const panels = scope ? {} : await layers(p);
    const tree = semantic.build(selected.nodes, INTERACTIVE, panels.layers || [], selected.truncated || panels.layers_truncated, args.text === true);
    const document = await documentId(p), url = p.url();
    if (beforeDocument !== document) throw fault("STALE_OBSERVATION", "Document changed during observation.");
    const identity = { document, url, mode: args.text ? "text" : "compact" };
    const previous = args._baselines?.[current];
    const feedback = semantic.present(tree, previous, mode, identity);
    return compactObservation({
      ...(await metadata()),
      url: p.url(),
      title: await bounded(p.title(), 3000).catch(() => ""),
      ...feedback,
      ...(!scope ? { _observation: { ...identity, ...tree, page_id: current,
        order: args._observation_order || 0, base_order: previous?.order || 0, scope: args._observation_scope } } : {}),
      ...(args.image ? { image: await capture(p, env.output, selected) } : {}),
    });
  };
  if (task.operation === "snapshot") {
    if (args.view === "tabs") return { value: await env.observe(null, "tabs") };
    const m = await env.observe(current);
    if (
      !m.page_id ||
      m.nativeWindow ||
      m.profilePicker ||
      m.health?.page === false
    )
      return {
        value: {
          ...m,
          tabs: undefined,
          ...(m.profilePicker
            ? { browser_ui: "profile_picker" }
            : m.nativeWindow
              ? { browser_ui: "native_window" }
              : m.page_id
                ? { browser_ui: "page_blocked" }
                : {}),
          ...(args.image
            ? {
                image_unavailable:
                  "Use screen_observe view=desktop for browser UI or a blocked page.",
              }
            : {}),
          window: undefined,
          nativeWindow: undefined,
          profilePicker: undefined,
        },
      };
    return { value: await observe(args.diff ? "diff" : "snapshot", args.ref) };
  }
  if (task.operation === "read") {
    const p = await getPage();
    return {
      value: compactObservation({ ...(await metadata()), url: p.url(), ...(await read(p, args)) }),
    };
  }
  for (const [index, a] of args.actions.entries()) {
    started = false;
    try {
      const timeout = (args.action_timeout_secs ?? 5) * 1000;
      if (a.do === "wait" && a.secs != null) {
        await new Promise(r => setTimeout(r, a.secs * 1000));
      } else if (a.do === "dialog") {
        policy.accept = a.accept;
        policy.text = a.text ?? undefined;
      } else if (a.do === "tab") {
        if (a.action === "new") {
          begin();
          current = await env.newPage();
          page = null;
          if (a.url) {
            const p = await getPage();
            await p.goto(a.url, {
              waitUntil: "domcontentloaded",
              timeout: 20000,
            });
          }
        } else {
          const id = a.page_id || current;
          if (!id)
            throw fault(
              "PAGE_NOT_FOUND",
              "Select a tab from browser_snapshot tabs.",
            );
          await env.tab(id, a.action, () => {
            begin();
          });
          if (a.action === "close" && id === current) {
            current = null;
            page = null;
          } else if (a.action === "focus") {
            current = id;
            page = null;
            observeTab = true;
          }
        }
      } else if (a.do === "desktop_type" || a.do === "window" || (["press", "scroll"].includes(a.do) && a.target === "desktop")) {
        if (a.do === "desktop_type") {
          begin();
          await env.desktop.type(a.text);
        } else if (a.do === "press") {
          begin();
          await env.desktop.press(a.key);
        } else if (a.do === "scroll") {
          env.desktop.validate(a);
          begin();
          await env.desktop.scroll(a);
        } else {
          begin();
          await env.window(a.mode, current);
        }
      } else if (
        (["click", "drag"].includes(a.do) && !a.ref) &&
        env.capture(a.image_id)?.source !== "page"
      ) {
        env.desktop.validate(a);
        if (a.do === "drag")
          env.desktop.validate({ ...a, x: a.to_x, y: a.to_y });
        begin();
        if (a.do === "drag") await env.desktop.drag(a);
        else
          await env.desktop.click({
            ...a,
            button: { left: 1, middle: 2, right: 3 }[a.button || "left"],
            double: a.double,
          });
      } else {
        const p = await getPage();
        if (a.do === "navigate" && (a.mode ?? "open") === "open") {
          begin();
          await p.goto(a.url, {
            waitUntil: "domcontentloaded",
            timeout: 20000,
          });
        } else if (a.do === "navigate" && ["back", "forward"].includes(a.mode)) {
          const s = await p.context().newCDPSession(p);
          let h;
          try {
            h = await bounded(s.send("Page.getNavigationHistory"), 3000);
          } finally {
            await s.detach().catch(() => {});
          }
          const at = h.currentIndex + (a.mode === "back" ? -1 : 1);
          if (at < 0 || at >= h.entries.length)
            throw fault("NO_HISTORY", "No history entry in that direction.");
          begin();
          await p[a.mode === "back" ? "goBack" : "goForward"]({
            waitUntil: "commit",
            timeout: 20000,
          });
          await bounded(
            p.waitForFunction(() => document.readyState !== "loading", null, {
              timeout: 20000,
            }),
            21000,
          );
        } else if (a.do === "navigate" && a.mode === "reload") {
          begin();
          await p.reload({ waitUntil: "domcontentloaded", timeout: 20000 });
        } else if (a.do === "wait") {
          await waitText(
              p,
              a.text ?? a.gone,
              a.text != null,
              (a.timeout_secs ?? 60) * 1000,
            );
        } else if (a.do === "press" && !a.ref) {
          begin();
          await p.keyboard.press(a.key);
        } else if (a.do === "scroll") {
          if (a.ref) {
            const l = await resolve(p, a.ref);
            begin();
            await l.scrollIntoViewIfNeeded({ timeout });
          } else if (a.until_text) {
            let found = false;
            for (let i = 0; i <= (a.max ?? 15); i++) {
              for (const f of p.frames()) {
                const l = f
                  .getByText(a.until_text, { exact: !!a.exact })
                  .first();
                if (await l.isVisible().catch(() => false)) {
                  begin();
                  await l.scrollIntoViewIfNeeded({ timeout });
                  found = true;
                  break;
                }
              }
              if (found) break;
              if (i === (a.max ?? 15)) break;
              begin();
              await p.mouse.wheel(0, a.dy ?? 800);
              await settle(p);
            }
            if (!found)
              throw fault(
                "NOT_FOUND",
                "Text not found after scrolling; inspect current content.",
              );
          } else {
            begin();
            await p.mouse.wheel(0, a.dy ?? 800);
          }
        } else if (["click", "drag"].includes(a.do) && !a.ref) {
          const image = env.capture(a.image_id);
          if (
            !image ||
            Date.now() - image.at > 60000 ||
            image.page_id !== current ||
            (await documentId(p)) !== image.document ||
            p.url() !== image.url
          )
            throw fault(
              "STALE_OBSERVATION",
              "Image expired or its document changed; capture again.",
            );
          const vp = await p.evaluate(() => ({
            w: innerWidth,
            h: innerHeight,
            sx: scrollX,
            sy: scrollY,
          }));
          if (
            vp.w !== image.viewport_width ||
            vp.h !== image.viewport_height ||
            Math.abs(vp.sx - image.sx) > 1 ||
            Math.abs(vp.sy - image.sy) > 1
          )
            throw fault(
              "STALE_OBSERVATION",
              "Page scrolled or resized; capture again.",
            );
          if (
            a.x >= image.width ||
            a.y >= image.height ||
            (a.do === "drag" &&
              (a.to_x >= image.width || a.to_y >= image.height))
          )
            throw fault(
              "INVALID_ARGUMENT",
              "Coordinates are outside the image.",
            );
          // Integer preview dimensions can round each axis differently. Use
          // the actual image/document ratio, especially near a long page's end.
          const scaleX = image.full_page ? image.width / image.content_width : image.scale,
            scaleY = image.full_page ? image.height / image.content_height : image.scale;
          const point = { x: a.x / scaleX, y: a.y / scaleY },
            end = { x: a.to_x / scaleX, y: a.to_y / scaleY };
          if (image.full_page) {
            const s = await bounded(p.context().newCDPSession(p), 2000);
            let content;
            try {
              content = (await bounded(s.send("Page.getLayoutMetrics"), 2000))
                .cssContentSize;
            } finally {
              await s.detach().catch(() => {});
            }
            if (
              Math.ceil(Math.max(content.width, vp.w)) !==
                image.content_width ||
              Math.ceil(Math.max(content.height, vp.h)) !== image.content_height
            )
              throw fault(
                "STALE_OBSERVATION",
                "Document layout size changed; capture again.",
              );
            if (
              a.do === "drag" &&
              (Math.abs(end.x - point.x) >= vp.w ||
                Math.abs(end.y - point.y) >= vp.h)
            )
              throw fault(
                "INVALID_ARGUMENT",
                "Drag endpoints must fit in one viewport; use element refs for longer drags or split the movement into shorter steps.",
              );
            const left =
                a.do === "drag" ? Math.min(point.x, end.x) : point.x,
              top = a.do === "drag" ? Math.min(point.y, end.y) : point.y;
            const right =
                a.do === "drag" ? Math.max(point.x, end.x) : point.x,
              bottom = a.do === "drag" ? Math.max(point.y, end.y) : point.y;
            if (
              left < vp.sx ||
              top < vp.sy ||
              right >= vp.sx + vp.w ||
              bottom >= vp.sy + vp.h
            ) {
              begin();
              await p.evaluate(
                ([x, y]) => scrollTo({ left: x, top: y, behavior: "instant" }),
                [
                  Math.max(0, (left + right - vp.w) / 2),
                  Math.max(0, (top + bottom - vp.h) / 2),
                ],
              );
              await settle(p);
            }
            const scroll = await p.evaluate(() => ({ x: scrollX, y: scrollY }));
            point.x -= scroll.x;
            point.y -= scroll.y;
            end.x -= scroll.x;
            end.y -= scroll.y;
          }
          begin();
          if (a.do === "click")
            await p.mouse.click(point.x, point.y, {
              button: a.button || "left",
              clickCount: a.double ? 2 : 1,
            });
          else {
            await p.mouse.move(point.x, point.y);
            await p.mouse.down();
            try {
              await p.mouse.move(end.x, end.y, { steps: 8 });
            } finally {
              await p.mouse.up();
            }
          }
        } else {
          const files = a.do === "upload" ? uploadFiles(a.paths) : null,
            l = await resolve(p, a.ref);
          if (a.do === "upload") {
            const direct = await l.evaluate(
              (e) =>
                e instanceof HTMLInputElement && e.type === "file"
                  ? { multiple: e.multiple }
                  : null,
              undefined,
              { timeout },
            );
            if (direct) {
              if (files.length > 1 && !direct.multiple)
                throw fault(
                  "INVALID_ARGUMENT",
                  "This file input accepts one file.",
                );
              begin();
              await l.setInputFiles(
                files.map((f) => f.path),
                { timeout },
              );
            } else {
              const pending = p
                .waitForEvent("filechooser", { timeout })
                .catch(() => null);
              begin();
              await l.click({ timeout });
              const fc = await pending;
              if (!fc)
                throw fault(
                  "NO_FILE_CHOOSER",
                  "The click did not open a file chooser.",
                );
              if (files.length > 1 && !fc.isMultiple())
                throw fault(
                  "INVALID_ARGUMENT",
                  "This file input accepts one file.",
                );
              await fc.setFiles(
                files.map((f) => f.path),
                { timeout },
              );
            }
          } else if (a.do === "download") {
            const pending = p
              .waitForEvent("download", { timeout: (a.timeout_secs ?? 30) * 1000 })
              .catch(() => null);
            begin();
            await l.click({ timeout });
            const d = await pending;
            if (!d)
              throw fault("NO_DOWNLOAD", "The click did not start a download.");
            const saved = await bounded(
              hooks.get(p.context()).save(d),
              (a.timeout_secs ?? 30) * 1000,
            );
            if (saved.error) throw fault("DOWNLOAD_FAILED", saved.error);
          } else if (a.do === "select") {
            const option = await l.evaluate(
              (e, value) => {
                if (!(e instanceof HTMLSelectElement)) return { invalid: true };
                const options = [...e.options],
                  matches = options.filter((o) => o.value === value),
                  selected = matches.length
                    ? matches
                    : options.filter((o) => o.label === value);
                return selected.length === 1
                  ? { value: selected[0].value }
                  : { count: selected.length };
              },
              a.value,
              { timeout },
            );
            if (option.invalid)
              throw fault(
                "INVALID_TARGET",
                "Select requires a native select element.",
              );
            if (option.value === undefined)
              throw fault(
                option.count ? "AMBIGUOUS_OPTION" : "NOT_FOUND",
                "Choose an exact, unambiguous option value or label.",
              );
            begin();
            await l.selectOption({ value: option.value }, { timeout });
          } else if (a.do === "drag") {
            const to = await resolve(p, a.to_ref);
            begin();
            await l.dragTo(to, { timeout });
          } else {
            begin();
            switch (a.do) {
              case "click":
                await l.click({
                  timeout,
                  button: a.button || "left",
                  clickCount: a.double ? 2 : 1,
                });
                break;
              case "fill":
                await l.fill(a.text, { timeout });
                if (a.submit) await l.press("Enter", { timeout });
                break;
              case "check":
                await l.setChecked(a.checked ?? true, { timeout });
                break;
              case "hover":
                await l.hover({ timeout });
                break;
              case "press":
                await l.press(a.key, { timeout });
                break;
              default:
                throw fault("INVALID_ARGUMENT", "Unsupported action.");
            }
          }
        }
        await settle(p);
      }
      mutated ||= started;
      done++;
      env.checkpoint({
        value: { completed: done, of: args.actions.length, page_id: current },
        side_effects: mutated ? "may_have_occurred" : "not_started",
      });
    } catch (e) {
      failure = {
        ...classify(e, page),
        step_number: index + 1,
        do: a.do,
        ...(a.ref ? { ref: a.ref } : {}),
      };
      break;
    }
  }
  let result = { completed: done, of: args.actions.length, page_id: current };
  if (
    (page || observeTab) &&
    current &&
    (!page || !page.isClosed()) &&
    !page?.__runcoveCrashed
  ) {
    try {
      result = {
        ...result,
        ...(await observe(failure ? "snapshot" : args.then || "diff")),
      };
    } catch (e) {
      Object.assign(result, await metadata().catch(() => ({})));
      result.observation_error = classify(e, page);
    }
  } else if (page || observeTab) Object.assign(result, await metadata().catch(() => ({})));
  for (const hook of hooks.values()) await hook.finish(result);

  if (failure) {
    const { step_number, do: action, ref, ...error } = failure;
    return {
      value: result,
      error,
      failed_step: { step_number, do: action, ...(ref ? { ref } : {}) },
      side_effects: started || mutated ? "may_have_occurred" : "not_started",
    };
  }
  return { value: result };
}
module.exports = { run, parse, classify, REF_RE };
