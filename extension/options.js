"use strict";

const $ = (id) => document.getElementById(id);
const DEFAULTS = { enabled: true, capture: true, privateWindows: false, browserFolder: false, minSizeMb: 0, excluded: [] };

async function load() {
  const s = await browser.storage.local.get(DEFAULTS);
  $("enabled").checked = s.enabled;
  $("capture").checked = s.capture;
  $("privateWindows").checked = s.privateWindows;
  $("browserFolder").checked = s.browserFolder;
  $("minSizeMb").value = s.minSizeMb;
  $("excluded").value = s.excluded.join("\n");
}

function save() {
  browser.storage.local.set({
    enabled: $("enabled").checked,
    capture: $("capture").checked,
    privateWindows: $("privateWindows").checked,
    browserFolder: $("browserFolder").checked,
    minSizeMb: Math.max(0, Number($("minSizeMb").value) || 0),
    excluded: $("excluded").value.split("\n").map((l) => l.trim()).filter(Boolean),
  });
}

for (const id of ["enabled", "capture", "privateWindows", "browserFolder", "minSizeMb", "excluded"]) {
  $(id).addEventListener("change", save);
}

async function status() {
  const el = $("status");
  if (!$("enabled").checked) {
    el.textContent = "Prism Downloads is off.";
    el.className = "";
    return;
  }
  try {
    await browser.runtime.sendMessage({ type: "ping" });
    el.textContent = "Connected to Prism.";
    el.className = "";
  } catch (e) {
    el.textContent = `Not connected to Prism: ${e.message}.`;
    el.className = "bad";
  }
}

// The popup's switch changes it too.
browser.storage.onChanged.addListener((c, area) => {
  if (area === "local" && c.enabled) {
    $("enabled").checked = c.enabled.newValue !== false;
    status();
  }
});
$("enabled").addEventListener("change", () => setTimeout(status, 100));

load().then(status);
