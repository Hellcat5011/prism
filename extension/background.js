// Prism Downloads: hands browser downloads to Prism's download manager.
//
// Talks to Prism through the native messaging host `prism_downloads` (prism-native-host,
// installed by Prism's deploy/install.sh), which passes download commands to prismd. A new
// browser download is offered to Prism first; only when Prism has taken it is the browser's
// own download cancelled and removed from its list. When Prism is not running, or the
// download cannot be repeated outside the browser (blob:, data:), the browser keeps it.

"use strict";

const HOST = "prism_downloads";

const DEFAULTS = {
  // Take browser downloads.
  capture: true,
  // Leave smaller downloads (MB) to the browser when their size is known; 0 = take all.
  minSizeMb: 0,
  // Sites whose downloads stay in the browser (host names; subdomains included).
  excluded: [],
  // Also take downloads from private windows.
  privateWindows: false,
  // Save into the folder the browser chose (its "always ask" dialog) instead of Prism's.
  browserFolder: false,
};

async function settings() {
  return Object.assign({}, DEFAULTS, await browser.storage.local.get(Object.keys(DEFAULTS)));
}

// ── Native messaging ────────────────────────────────────────────────────────

let port = null;
let nextId = 1;
const pending = new Map();

function connect() {
  port = browser.runtime.connectNative(HOST);
  port.onMessage.addListener((m) => {
    const p = pending.get(m.id);
    if (!p) return;
    pending.delete(m.id);
    if (m.ok) p.resolve(m.data);
    else p.reject(new Error(m.error || "Prism refused"));
  });
  port.onDisconnect.addListener((p) => {
    const why = (p && p.error && p.error.message) || "";
    const err = new Error(
      /not found|No such native application/i.test(why)
        ? "Prism's helper (prism-native-host) is not installed"
        : "Lost the connection to Prism"
    );
    for (const w of pending.values()) w.reject(err);
    pending.clear();
    port = null;
  });
}

function call(message, timeoutMs = 10000) {
  return new Promise((resolve, reject) => {
    try {
      if (!port) connect();
      const id = nextId++;
      pending.set(id, { resolve, reject });
      port.postMessage(Object.assign({ id }, message));
      setTimeout(() => {
        if (pending.delete(id)) reject(new Error("Prism did not answer"));
      }, timeoutMs);
    } catch (e) {
      reject(e);
    }
  });
}

const prism = (action) => call({ command: { target: "download", action } });

// ── Requests ────────────────────────────────────────────────────────────────

function isHttp(url) {
  return /^https?:\/\//i.test(url || "");
}

function hostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return "";
  }
}

function isExcluded(url, list) {
  const h = hostOf(url);
  return !!h && list.some((e) => {
    const x = e.trim().toLowerCase().replace(/^\*\./, "");
    return x && (h === x || h.endsWith("." + x));
  });
}

// The Cookie header the browser would send to `url` (from the same container).
async function cookieHeader(url, storeId) {
  try {
    const q = { url, firstPartyDomain: null, partitionKey: {} };
    if (storeId) q.storeId = storeId;
    const cookies = await browser.cookies.getAll(q);
    return cookies.length ? cookies.map((c) => `${c.name}=${c.value}`).join("; ") : null;
  } catch {
    return null;
  }
}

function baseName(path) {
  const n = (path || "").split(/[\\/]/).pop();
  return n || null;
}

function dirName(path) {
  const i = (path || "").lastIndexOf("/");
  return i > 0 ? path.slice(0, i) : null;
}

async function send(req) {
  const id = await prism({ add: req });
  refreshSoon();
  return id;
}

// ── Taking browser downloads ────────────────────────────────────────────────

browser.downloads.onCreated.addListener(async (item) => {
  const s = await settings();
  const url = item.finalUrl || item.url;
  if (!s.capture || item.state !== "in_progress" || !isHttp(url)) return;
  if (item.incognito && !s.privateWindows) return;
  if (isExcluded(url, s.excluded) || (item.referrer && isExcluded(item.referrer, s.excluded))) return;
  if (s.minSizeMb > 0 && item.totalBytes > 0 && item.totalBytes < s.minSizeMb * 1e6) return;
  const req = {
    url,
    referer: isHttp(item.referrer) ? item.referrer : null,
    cookie: await cookieHeader(url, item.cookieStoreId),
    user_agent: navigator.userAgent,
    name: baseName(item.filename),
    dir: s.browserFolder ? dirName(item.filename) : null,
    // Prism shows its confirmation card (when on in Prism's Settings).
    ask: true,
  };
  try {
    await send(req);
  } catch (e) {
    // Prism is not there: the browser carries on with its own download.
    console.info("Prism Downloads: left to the browser:", e.message);
    return;
  }
  try {
    await browser.downloads.cancel(item.id);
    await browser.downloads.erase({ id: item.id });
  } catch {
    // Already finished or gone.
  }
});

// ── Link menu ───────────────────────────────────────────────────────────────

browser.runtime.onInstalled.addListener(() => {
  browser.contextMenus.create({
    id: "prism-download",
    title: "Download with Prism",
    contexts: ["link", "video", "audio", "image"],
  });
});

browser.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId !== "prism-download") return;
  const url = info.linkUrl || info.srcUrl;
  if (!isHttp(url)) return;
  try {
    await send({
      url,
      referer: isHttp(tab && tab.url) ? tab.url : null,
      cookie: await cookieHeader(url, tab && tab.cookieStoreId),
      user_agent: navigator.userAgent,
      ask: true,
    });
  } catch (e) {
    browser.action.setBadgeText({ text: "!" });
    browser.action.setTitle({ title: `Prism Downloads: ${e.message}` });
  }
});

// ── Videos on the page ──────────────────────────────────────────────────────
//
// Responses in each tab that are HLS playlists (.m3u8) or whole video/audio files. Master
// playlists are read for their qualities; their variant and audio playlists are not listed
// on their own. The content script shows a button over the page's videos with these, and
// the popup lists them.

// tabId -> Map(key -> entry)
const media = new Map();
// Variant playlists of masters already listed (not shown separately).
const children = new Set();
let nextMediaId = 1;

// Sites that split video and audio into many small requests (not downloadable this way).
const BLOCKED = ["youtube.com", "googlevideo.com", "youtube-nocookie.com", "netflix.com", "nflxvideo.net", "disneyplus.com", "primevideo.com"];

function header(headers, name) {
  const h = (headers || []).find((x) => x.name.toLowerCase() === name);
  return h ? h.value : "";
}

function qualityLabel(v) {
  if (v.height) return `${v.height}p`;
  if (v.bandwidth) return `${Math.round(v.bandwidth / 1000)} kbps`;
  return "Video";
}

// Master playlist: [{ url, height, bandwidth }]; a media playlist: { duration }.
function parseM3u8(text, base) {
  const lines = text.split(/\r?\n/).map((l) => l.trim()).filter(Boolean);
  const variants = [];
  let duration = 0;
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    if (l.startsWith("#EXT-X-STREAM-INF:")) {
      const res = /RESOLUTION=(\d+)x(\d+)/.exec(l);
      const bw = /[^-]BANDWIDTH=(\d+)/.exec("," + l.slice(18));
      const uri = lines.slice(i + 1).find((x) => !x.startsWith("#"));
      if (uri) variants.push({ url: new URL(uri, base).href, height: res ? Number(res[2]) : 0, bandwidth: bw ? Number(bw[1]) : 0 });
    } else if (l.startsWith("#EXTINF:")) {
      duration += parseFloat(l.slice(8)) || 0;
    }
  }
  // Highest first, one per height.
  variants.sort((a, b) => b.height - a.height || b.bandwidth - a.bandwidth);
  const seen = new Set();
  return { variants: variants.filter((v) => (v.height ? !seen.has(v.height) && seen.add(v.height) : true)), duration };
}

function tabMedia(tabId) {
  if (!media.has(tabId)) media.set(tabId, new Map());
  return media.get(tabId);
}

function changed(tabId) {
  browser.tabs.sendMessage(tabId, { type: "media-changed" }).catch(() => {});
}

// Headers the page sent with a request (by requestId), kept until its answer arrives. Many
// stream hosts refuse a playlist without the player's Origin and Referer, so Prism sends the
// same ones.
const sent = new Map();

browser.webRequest.onSendHeaders.addListener(
  (d) => {
    if (d.tabId < 0) return;
    const referer = header(d.requestHeaders, "referer");
    const origin = header(d.requestHeaders, "origin");
    if (!referer && !origin) return;
    sent.set(d.requestId, { referer, origin });
    if (sent.size > 1000) sent.delete(sent.keys().next().value);
  },
  { urls: ["<all_urls>"], types: ["media", "xmlhttprequest", "object", "other"] },
  ["requestHeaders"]
);

// A playlist as the page sees it: read by the content script of the frame that played it
// (content.fetch sends that page's Origin, Referer and cookies), else by this script.
async function readPlaylist(d) {
  try {
    const text = await browser.tabs.sendMessage(d.tabId, { type: "read-playlist", url: d.url }, { frameId: d.frameId });
    if (typeof text === "string") return text;
  } catch {
    // No content script in that frame.
  }
  const r = await fetch(d.url, { credentials: "include" });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.text();
}

// What Prism sends for one of the page's media requests: the headers the page used.
async function mediaRequest(entry, tab) {
  const seen = entry.headers || {};
  return {
    url: entry.url,
    referer: isHttp(seen.referer) ? seen.referer : isHttp(entry.pageUrl) ? entry.pageUrl : isHttp(tab && tab.url) ? tab.url : null,
    cookie: await cookieHeader(entry.url, tab && tab.cookieStoreId),
    user_agent: navigator.userAgent,
    headers: isHttp(seen.origin) ? [["Origin", seen.origin]] : [],
  };
}

// Qualities and length of a playlist: read in the page's frame, else by Prism with the
// page's headers (what the download itself will send).
async function playlistInfo(d, entry) {
  try {
    return parseM3u8(await readPlaylist(d), d.url);
  } catch {
    const tab = await browser.tabs.get(d.tabId).catch(() => null);
    const info = await prism({ inspect: await mediaRequest(entry, tab) });
    const variants = info.variants.sort((a, b) => b.height - a.height || b.bandwidth - a.bandwidth);
    const seen = new Set();
    return { variants: variants.filter((v) => (v.height ? !seen.has(v.height) && seen.add(v.height) : true)), duration: info.duration };
  }
}

async function addHls(d, headers) {
  const list = tabMedia(d.tabId);
  if (children.has(d.url) || list.has(d.url)) return;
  const entry = { id: nextMediaId++, kind: "hls", url: d.url, variants: [], duration: 0, pageUrl: d.documentUrl || d.originUrl || "", headers, found: Date.now() };
  list.set(d.url, entry);
  try {
    const info = await playlistInfo(d, entry);
    entry.variants = info.variants;
    entry.duration = info.duration;
    for (const v of info.variants) {
      children.add(v.url);
      // A variant listed before its master was seen: the master covers it.
      for (const [k, e] of list) if (e.url === v.url && e !== entry) list.delete(k);
    }
    // Players fetch the same playlist again under new signed URLs: one entry per stream
    // (the newest, whose signature is freshest).
    if (!entry.variants.length && entry.duration) {
      for (const [k, e] of list) {
        if (e !== entry && e.kind === "hls" && !e.variants.length && Math.abs(e.duration - entry.duration) < 0.5 && hostOf(e.url) === hostOf(entry.url)) list.delete(k);
      }
    }
  } catch {
    // Listed as it is; Prism reads it again.
  }
  changed(d.tabId);
}

browser.webRequest.onHeadersReceived.addListener(
  (d) => {
    const headers = sent.get(d.requestId);
    sent.delete(d.requestId);
    if (d.tabId < 0 || !isHttp(d.url) || d.statusCode >= 400) return;
    if (BLOCKED.some((b) => hostOf(d.url) === b || hostOf(d.url).endsWith("." + b))) return;
    const type = header(d.responseHeaders, "content-type").toLowerCase();
    const path = (() => { try { return new URL(d.url).pathname.toLowerCase(); } catch { return ""; } })();
    if (/mpegurl/.test(type) || path.endsWith(".m3u8")) {
      addHls(d, headers);
      return;
    }
    const isMedia = /^(video|audio)\//.test(type) || /\.(mp4|webm|mkv|mov|m4v|ogv|mp3|m4a|flac|opus|oga)$/.test(path);
    // Stream segments and subtitles are parts, not files.
    if (!isMedia || /\.(ts|m4s|m4f|aac|vtt|cmfv|cmfa)$/.test(path) || /mp2t/.test(type)) return;
    const range = header(d.responseHeaders, "content-range");
    const size = range ? Number(range.split("/")[1]) || 0 : Number(header(d.responseHeaders, "content-length")) || 0;
    // Short sounds and previews are not worth a button.
    if (size && size < 512 * 1024) return;
    const key = d.url.replace(/([?&])(range|bytes)=[^&]*/g, "$1");
    const list = tabMedia(d.tabId);
    if (list.has(key)) return;
    list.set(key, { id: nextMediaId++, kind: "file", url: d.url, size, type: type.split(";")[0], pageUrl: d.documentUrl || d.originUrl || "", headers, found: Date.now() });
    changed(d.tabId);
  },
  { urls: ["<all_urls>"], types: ["media", "xmlhttprequest", "object", "other"] },
  ["responseHeaders"]
);

browser.tabs.onUpdated.addListener((tabId, info) => {
  // A new page: its videos are not those of the last one.
  if (info.status === "loading" && info.url) {
    media.delete(tabId);
    changed(tabId);
  }
});
browser.tabs.onRemoved.addListener((tabId) => media.delete(tabId));

function mediaList(tabId) {
  return [...(media.get(tabId) || new Map()).values()].sort((a, b) => a.found - b.found);
}

async function grab(tabId, entryId, variantUrl) {
  const entry = mediaList(tabId).find((e) => e.id === entryId);
  if (!entry) throw new Error("That video is gone; play it again");
  const tab = await browser.tabs.get(tabId);
  const title = (tab.title || "video").replace(/[\\/:*?"<>|]+/g, " ").trim().slice(0, 150);
  const req = { ...(await mediaRequest(entry, tab)), ask: true };
  if (entry.kind === "hls") {
    req.kind = "hls";
    req.variant = variantUrl || null;
    req.name = title;
  } else {
    const ext = (/\.([a-z0-9]{2,4})$/i.exec(new URL(entry.url).pathname) || [])[1] || (entry.type.split("/")[1] || "mp4");
    req.name = `${title}.${ext}`;
  }
  return send(req);
}

// ── Toolbar badge: downloads running ────────────────────────────────────────

let timer = null;

async function refresh() {
  timer = null;
  let list;
  try {
    list = await prism("list");
  } catch {
    browser.action.setBadgeText({ text: "" });
    return;
  }
  const active = list.filter((d) => ["running", "probing", "queued"].includes(d.state)).length;
  browser.action.setBadgeText({ text: active ? String(active) : "" });
  browser.action.setTitle({ title: "Prism Downloads" });
  if (active) refreshSoon(2000);
}

function refreshSoon(ms = 500) {
  if (!timer) timer = setTimeout(refresh, ms);
}

browser.action.setBadgeBackgroundColor({ color: "#5ab8ff" });

// ── The popup asks through here ─────────────────────────────────────────────

browser.runtime.onMessage.addListener((msg, sender) => {
  switch (msg && msg.type) {
    case "ping":
      return call({ ping: true });
    case "list":
      return prism("list");
    case "action":
      // { action: "pause" | "resume" | "cancel" | "remove" | "open" | "show", id }
      return prism({ [msg.action]: msg.id }).then((r) => (refreshSoon(), r));
    case "clear":
      return prism("clear");
    case "add":
      return send({ url: msg.url, user_agent: navigator.userAgent });
    // Videos found in a tab (the sender's, or `tabId` from the popup).
    case "media":
      return Promise.resolve(mediaList(msg.tabId != null ? msg.tabId : sender.tab && sender.tab.id).map((e) => ({ ...e, labels: (e.variants || []).map(qualityLabel) })));
    case "grab":
      return grab(msg.tabId != null ? msg.tabId : sender.tab.id, msg.id, msg.variant);
  }
  return undefined;
});

refreshSoon(1000);
