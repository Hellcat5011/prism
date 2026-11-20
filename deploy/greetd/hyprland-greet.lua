-- Minimal hardened Hyprland config for the greetd greeter session (Lua config, Hyprland 0.55+).
hl.config({
  misc = {
    disable_hyprland_logo = true,
    disable_splash_rendering = true,
  },
})

hl.on("hyprland.start", function()
  hl.exec_cmd("prism-lock --greeter")
end)
