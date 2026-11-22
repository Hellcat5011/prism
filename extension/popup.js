"use strict";

const $ = (id) => document.getElementById(id);

const ICONS = {
  pause: "M6 5h4v14H6zM14 5h4v14h-4z",
  resume: "M7 4.5v15L19.5 12z",
  cancel: "M6.4 5 12 10.6 17.6 5 19 6.4 13.4 12 19 17.6 17.6 19 12 13.4 6.4 19 5 17.6 10.6 12 5 6.4z",
  open: "M14 3h7v7h-2V6.4l-9.3 9.3-1.4-1.4L17.6 5H14zM5 5h6v2H5v12h12v-6h2v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2z",
  show: "M3 6a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  remove: "M9 3h6l1 2h4v2H4V5h4zM6 9h12l-1 12H7z",
};

const TITLES = { pause: "Pause", resume: "Resume", cancel: "Cancel", open: "Open", show: "Show in folder", remove: "Remove from list" };

function size(b) {
  if (b >= 1e9) return (b / 1e9).toFixed(2) + " GB";
  if (b >= 1e6) return (b / 1e6).toFixed(1) + " MB";
  return Math.max(1, Math.round(b / 1e3)) + " KB";
}

function eta(d) {
  if (!d.size || !d.rate) return "";
  const s = Math.round((d.size - d.done) / d.rate);
  if (s < 60) return `${s} s left`;
  if (s < 3600) return `${Math.round(s / 60)} min left`;
  return `${(s / 3600).toFixed(1)} h left`;
}

function detail(d) {
  const amount = d.size ? `${size(d.done)} of ${size(d.size)}` : size(d.done);
  switch (d.state) {
    case "running":
      return [amount, d.rate ? `${size(d.rate)}/s` : "", eta(d), d.connections > 1 ? `${d.connections} connections` : ""].filter(Boolean).join(" · ");
    case "probing":
      return "Starting…";
    case "queued":
      return "Waiting for other downloads";
    case "paused":
      return `Paused · ${amount}`;
    case "done":
      return d.size ? size(d.size) : "Done";
    case "failed":
      return d.error || "Failed";
    case "cancelled":
      return "Cancelled";
  }
  return d.state;
}

function actionsFor(d) {
  switch (d.state) {
    case "running":
    case "queued":
      return ["pause", "cancel"];
    case "probing":
      return ["cancel"];
    case "paused":
    case "failed":
      return ["resume", "cancel", "remove"];
    case "done":
      return ["open", "show", "remove"];
    default:
      return ["remove"];
  }
}

function icon(name) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  const p = document.createElementNS(ns, "path");
  p.setAttribute("d", ICONS[name]);
  svg.appendChild(p);
  return svg;
}

function item(d) {
  const li = document.createElement("li");
  li.className = d.state;
  const row = document.createElement("div");
  row.className = "row";
  const name = document.createElement("span");
  name.className = "name";
  name.textContent = d.name;
  name.title = d.path || d.url;
  const actions = document.createElement("span");
  actions.className = "actions";
  for (const a of actionsFor(d)) {
    const b = document.createElement("button");
    b.title = TITLES[a];
    b.appendChild(icon(a));
    b.addEventListener("click", () => act(a, d.id));
    actions.appendChild(b);
  }
  row.append(name, actions);
  const bar = document.createElement("div");
  bar.className = "bar" + (d.size ? "" : " unknown");
  const fill = document.createElement("span");
  fill.style.width = d.size ? `${Math.min(100, (d.done * 100) / d.size)}%` : "30%";
  bar.appendChild(fill);
  const info = document.createElement("div");
  info.className = "detail";
  info.textContent = detail(d);
  li.append(row, bar, info);
  return li;
}

function message(text) {
  $("message").hidden = !text;
  $("message").textContent = text || "";
}

let last = "";

async function load() {
  if (!on) return;
  let list;
  try {
    list = await browser.runtime.sendMessage({ type: "list" });
  } catch (e) {
    if (!on) return;
    message(`${e.message}. Downloads stay in Firefox until Prism is running.`);
    $("list").replaceChildren();
    $("empty").hidden = true;
    return;
  }
  message("");
  const key = JSON.stringify(list);
  if (key === last) return;
  last = key;
  $("list").replaceChildren(...list.map(item));
  $("empty").hidden = list.length > 0;
}

async function act(action, id) {
  try {
    await browser.runtime.sendMessage({ type: "action", action, id });
  } catch (e) {
    message(e.message);
  }
  load();
}

$("add").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const url = $("url").value.trim();
  if (!/^https?:\/\//i.test(url)) {
    message("Only http and https links can be downloaded.");
    return;
  }
  try {
    await browser.runtime.sendMessage({ type: "add", url });
    $("url").value = "";
    message("");
  } catch (e) {
    message(e.message);
  }
  load();
});

$("clear").addEventListener("click", async () => {
  try {
    await browser.runtime.sendMessage({ type: "clear" });
  } catch (e) {
    message(e.message);
  }
  load();
});

$("options").addEventListener("click", () => {
  browser.runtime.openOptionsPage();
  window.close();
});

// The master switch: off, the extension does nothing, and the popup only says so.
let on = true;

function showOn(value) {
  on = value;
  $("enabled").checked = on;
  for (const id of ["add", "list", "clear"]) $(id).hidden = !on;
  if (!on) {
    $("videos").hidden = true;
    $("empty").hidden = true;
    last = "";
    lastVideos = "";
    message("Prism Downloads is off: downloads stay in Firefox and no videos are looked for.");
  } else {
    message("");
    load();
    loadVideos();
  }
}

browser.storage.local.get({ enabled: true }).then((s) => showOn(s.enabled));
$("enabled").addEventListener("change", (ev) => {
  browser.storage.local.set({ enabled: ev.target.checked });
  showOn(ev.target.checked);
});

// ── Videos found in the active tab ─────────────────────────────────────────

let activeTab = null;
let lastVideos = "";

function mb(b) {
  return b >= 1e9 ? (b / 1e9).toFixed(1) + " GB" : Math.max(1, Math.round(b / 1e6)) + " MB";
}

async function loadVideos() {
  if (!on) return;
  if (activeTab == null) {
    const [tab] = await browser.tabs.query({ active: true, currentWindow: true });
    activeTab = tab ? tab.id : -1;
  }
  let list = [];
  try {
    list = await browser.runtime.sendMessage({ type: "media", tabId: activeTab });
  } catch {
    list = [];
  }
  if (!on) return;
  const key = JSON.stringify(list.map((e) => [e.id, e.labels, e.size]));
  if (key === lastVideos) return;
  lastVideos = key;
  $("videos").hidden = list.length === 0;
  $("videoList").replaceChildren(
    ...list.map((e) => {
      const li = document.createElement("li");
      const what = document.createElement("div");
      what.className = "what";
      const b = document.createElement("b");
      b.textContent = e.kind === "hls" ? "Video stream" : (e.type.split("/")[1] || "Video").toUpperCase() + " file";
      const small = document.createElement("small");
      small.textContent = e.kind === "hls" ? (e.duration ? `${Math.round(e.duration / 60)} min` : "HLS") : e.size ? mb(e.size) : "";
      what.append(b, small);
      li.appendChild(what);
      let select = null;
      if (e.kind === "hls" && e.variants.length > 1) {
        select = document.createElement("select");
        e.variants.forEach((v, i) => select.add(new Option(e.labels[i], v.url)));
        li.appendChild(select);
      }
      const go = document.createElement("button");
      go.textContent = "Download";
      go.addEventListener("click", async () => {
        const variant = select ? select.value : e.variants && e.variants[0] ? e.variants[0].url : null;
        try {
          await browser.runtime.sendMessage({ type: "grab", tabId: activeTab, id: e.id, variant });
          go.textContent = "Sent";
        } catch (err) {
          message(err.message);
        }
      });
      li.appendChild(go);
      return li;
    })
  );
}

load();
loadVideos();
setInterval(() => {
  load();
  loadVideos();
}, 1000);
