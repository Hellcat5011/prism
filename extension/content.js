// Prism Downloads: a "Download video" button over the page's videos while the pointer is on
// them, when the background script found something to download in this tab (HLS playlists
// with their qualities, or whole video files). Lives in a closed shadow root so the page's
// styles cannot reach it.

(() => {
  "use strict";
  if (window.__prismDownloads) return;
  window.__prismDownloads = true;

  let entries = [];
  let host = null;
  let root = null;
  let target = null;
  let hideTimer = null;
  let menuOpen = false;

  async function refresh() {
    try {
      entries = (await browser.runtime.sendMessage({ type: "media" })) || [];
    } catch {
      entries = [];
    }
  }

  browser.runtime.onMessage.addListener((m) => {
    if (m && m.type === "media-changed") refresh();
  });

  function size(b) {
    if (!b) return "";
    if (b >= 1e9) return (b / 1e9).toFixed(1) + " GB";
    return Math.max(1, Math.round(b / 1e6)) + " MB";
  }

  // What can be downloaded for this video: its own file first, then everything found.
  function choices(video) {
    const src = video.currentSrc || video.src || "";
    const own = entries.filter((e) => e.kind === "file" && e.url === src);
    const list = own.length ? own : entries;
    const out = [];
    for (const e of list) {
      if (e.kind === "hls" && e.variants && e.variants.length) {
        e.variants.forEach((v, i) => {
          const est = v.bandwidth && e.duration ? size((v.bandwidth / 8) * e.duration) : "";
          out.push({ id: e.id, variant: v.url, label: e.labels[i], detail: est ? `about ${est}` : "Stream" });
        });
      } else if (e.kind === "hls") {
        out.push({ id: e.id, variant: null, label: "Video", detail: "Stream" });
      } else {
        out.push({ id: e.id, variant: null, label: (e.type.split("/")[1] || "file").toUpperCase(), detail: size(e.size) });
      }
    }
    return out;
  }

  function build() {
    host = document.createElement("div");
    host.style.cssText = "all:initial;position:fixed;z-index:2147483647;top:0;left:0;";
    root = host.attachShadow({ mode: "closed" });
    const style = document.createElement("style");
    style.textContent = `
        :host { all: initial; }
        .wrap { font: 600 13px/1.2 system-ui, sans-serif; color: #fff; }
        button.main {
          display: flex; align-items: center; gap: 7px; padding: 7px 12px 7px 10px;
          border: 1px solid rgba(255,255,255,.18); border-radius: 999px; cursor: pointer;
          background: rgba(16,18,24,.82); color: #fff; font: inherit;
          box-shadow: 0 4px 16px rgba(0,0,0,.35); backdrop-filter: blur(8px);
          transition: background .15s, transform .15s;
        }
        button.main:hover { background: rgba(24,28,38,.95); transform: translateY(-1px); }
        svg { width: 16px; height: 16px; stroke: #5ab8ff; fill: none; stroke-width: 2.2; stroke-linecap: round; stroke-linejoin: round; }
        .menu {
          margin-top: 6px; min-width: 210px; padding: 5px; border-radius: 12px;
          background: rgba(16,18,24,.95); border: 1px solid rgba(255,255,255,.14);
          box-shadow: 0 8px 28px rgba(0,0,0,.45);
        }
        .menu[hidden] { display: none; }
        .item {
          display: flex; justify-content: space-between; gap: 16px; width: 100%; padding: 8px 10px;
          border: 0; border-radius: 8px; background: none; color: #fff; font: inherit; cursor: pointer; text-align: left;
        }
        .item:hover { background: linear-gradient(90deg, rgba(90,184,255,.28), rgba(192,124,255,.22)); }
        .item span { color: rgba(255,255,255,.6); font-weight: 400; }
        .note { padding: 6px 12px; font-weight: 400; color: rgba(255,255,255,.85); }
      `;
    const wrap = document.createElement("div");
    wrap.className = "wrap";
    const main = document.createElement("button");
    main.className = "main";
    main.title = "Download with Prism";
    const ns = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(ns, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", "M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4M7 10l5 5 5-5M12 15V3");
    svg.appendChild(path);
    const label = document.createElement("span");
    label.className = "label";
    label.textContent = "Download video";
    main.append(svg, label);
    const menu = document.createElement("div");
    menu.className = "menu";
    menu.hidden = true;
    wrap.append(main, menu);
    root.append(style, wrap);
    root.querySelector("button.main").addEventListener("click", (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      if (!target) return;
      const list = choices(target);
      if (list.length === 1) {
        send(list[0]);
        return;
      }
      toggleMenu(list);
    });
    host.addEventListener("mouseenter", () => clearTimeout(hideTimer));
    host.addEventListener("mouseleave", () => hideSoon());
  }

  function toggleMenu(list) {
    const menu = root.querySelector(".menu");
    menuOpen = menu.hidden;
    menu.replaceChildren();
    for (const c of list) {
      const b = document.createElement("button");
      b.className = "item";
      b.textContent = c.label;
      const d = document.createElement("span");
      d.textContent = c.detail;
      b.appendChild(d);
      b.addEventListener("click", (ev) => {
        ev.stopPropagation();
        send(c);
      });
      menu.appendChild(b);
    }
    menu.hidden = !menuOpen;
  }

  async function send(c) {
    const label = root.querySelector(".label");
    root.querySelector(".menu").hidden = true;
    menuOpen = false;
    try {
      await browser.runtime.sendMessage({ type: "grab", id: c.id, variant: c.variant });
      label.textContent = "Sent to Prism";
    } catch (e) {
      label.textContent = e.message || "Prism is not running";
    }
    setTimeout(() => {
      label.textContent = "Download video";
    }, 2500);
  }

  function show(video) {
    if (!host) build();
    const parent = document.fullscreenElement || document.documentElement;
    if (host.parentNode !== parent) parent.appendChild(host);
    target = video;
    const r = video.getBoundingClientRect();
    host.style.display = "block";
    const bw = root.querySelector("button.main").getBoundingClientRect().width || 160;
    host.style.top = `${Math.max(4, r.top + 12)}px`;
    host.style.left = `${Math.max(4, r.right - 12 - bw)}px`;
    clearTimeout(hideTimer);
  }

  function hideSoon() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (menuOpen || !host) return;
      host.style.display = "none";
      target = null;
    }, 1500);
  }

  // Players lay their controls over the video, so look at what is under the pointer.
  let lastMove = -Infinity;
  document.addEventListener(
    "mousemove",
    (ev) => {
      const now = performance.now();
      if (now - lastMove < 120) return;
      lastMove = now;
      if (!entries.length) return;
      const video = [...document.querySelectorAll("video")].find((v) => {
        const r = v.getBoundingClientRect();
        return r.width >= 200 && r.height >= 120 && ev.clientX >= r.left && ev.clientX <= r.right && ev.clientY >= r.top && ev.clientY <= r.bottom;
      });
      if (video) show(video);
      else if (target && !menuOpen) hideSoon();
    },
    { passive: true, capture: true }
  );

  refresh();
})();
