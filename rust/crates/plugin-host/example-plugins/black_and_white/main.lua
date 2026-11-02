-- Black and white: an example plugin. docs/writing-plugins.md walks through it.

local plugin = {}

-- What gets saved: whether it's on and how strong it is
local on = false
local strength = 100

function plugin.card()
  return {
    file = "card.slint",
    component = "BlackAndWhiteCard",
    region = "right",
    props = { on = on, strength = strength },
  }
end

function plugin.on_ui(event)
  if event.callback == "toggled" then
    on = event.args[1] == true
  elseif event.callback == "strength_moved" then
    strength = math.floor(event.args[1] + 0.5)
  else
    return
  end
  host.refresh_frame_step()
  host.schedule_save()
end

function plugin.frame_step()
  if not on then
    return nil
  end
  return { shader = "shaders/grey.wgsl", uniforms = { strength = strength / 100 } }
end

function plugin.get_config()
  return { on = on, strength = strength }
end

function plugin.set_config(cfg)
  on = cfg.on == true
  if math.type(cfg.strength) == "integer" and cfg.strength >= 0 and cfg.strength <= 100 then
    strength = cfg.strength
  else
    strength = 100
  end
  host.set_ui_props("card", { on = on, strength = strength })
  host.refresh_frame_step()
end

return plugin
