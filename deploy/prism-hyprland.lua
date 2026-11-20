-- ─────────────────────────────────────────────────────────────────────────
-- Prism (Rust) — Hyprland configuration snippet (Lua config, Hyprland 0.55+)
--
-- Link this file into ~/.config/hypr and require it from hyprland.lua:
--     ln -s /path/to/prism/deploy/prism-hyprland.lua ~/.config/hypr/prism.lua
--     require("prism")
-- ─────────────────────────────────────────────────────────────────────────

-- prismd is started by the systemd user unit (deploy/prismd.service); do not
-- also exec it here (a second launcher orphaned a duplicate desktop layer).

-- ── Blur Rules for Layer Surfaces ─────────────────────────────────────────
-- Only card-sized surfaces are blurred. prism-scrim (full-screen dim),
-- prism-desktop, prism-desktop-edit and prism-wallpaper must stay un-blurred.
hl.layer_rule({
    match = { namespace = "^(prism-modal|prism-notification|prism-osd)$" },
    blur = true,
    ignore_alpha = 0.01,
})

-- ── Shell Keybindings ────────────────────────────────────────────────────
local binds = {
    { "SUPER + space",         "prism-ctl launcher toggle" },
    { "SUPER + SHIFT + space", "prism-ctl search toggle" },
    { "SUPER + ALT + W",       "prism-ctl wallpaper toggle" },
    { "SUPER + ALT + V",       "prism-ctl clipboard toggle" },
    { "Print",                 "prism-ctl screenshot toggle" },
    { "SHIFT + Print",         "prism-ctl screenshot screen" },
    { "SUPER + SHIFT + R",     "prism-ctl screenshot record" },
    { "SUPER + ALT + E",       "prism-ctl emoji toggle" },
    { "SUPER + ALT + C",       "prism-ctl colorpicker toggle" },
    { "SUPER + ALT + N",       "prism-ctl notif toggle" },
    { "SUPER + ALT + comma",   "prism-ctl settings toggle" },
    { "SUPER + ALT + P",       "prism-ctl power toggle" },
    { "SUPER + ALT + slash",   "prism-ctl keybinds toggle" },
    { "SUPER + ALT + Delete",  "prism-ctl lock lock" },
}
for _, b in ipairs(binds) do
    hl.bind(b[1], hl.dsp.exec_cmd(b[2]))
end

-- ── Hardware OSD ─────────────────────────────────────────────────────────
-- Repeat while held, and keep working while the screen is locked.
local osd = {
    { "XF86AudioRaiseVolume",  "prism-ctl osd volume +5" },
    { "XF86AudioLowerVolume",  "prism-ctl osd volume -5" },
    { "XF86MonBrightnessUp",   "prism-ctl osd brightness +5" },
    { "XF86MonBrightnessDown", "prism-ctl osd brightness -5" },
}
for _, b in ipairs(osd) do
    hl.bind(b[1], hl.dsp.exec_cmd(b[2]), { locked = true, repeating = true })
end
