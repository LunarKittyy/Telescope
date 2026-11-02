-- Stream alerts: an example of a plugin that follows events and uses a host service. docs/writing-plugins.md explains it.

local plugin = { events = {} }

-- Streams that dropped and haven't come back yet, by source id
local lost = {}
local drops = 0
local when_back = true

function plugin.events.stream_lost(e)
  if lost[e.source_id] then
    return
  end
  lost[e.source_id] = true
  drops = drops + 1
  host.send_notification(tr("Stream dropped"), tr("Telescope is looking for a way back to the phone."))
end

function plugin.events.stream_connected(e)
  if not lost[e.source_id] then
    return
  end
  lost[e.source_id] = nil
  if when_back then
    host.send_notification(tr("Stream is back"), tr("The phone's picture is coming in again."), false)
  end
end

function plugin.events.stream_stopped()
  lost = {}
end

function plugin.actions()
  return { { id = "stream_alerts.test", label = tr("Show a test alert"), group = tr("Stream alerts") } }
end

function plugin.run_action(id)
  if id == "stream_alerts.test" then
    host.send_notification(tr("Stream alerts"), tr("This is what an alert looks like."), false)
    return tr("Stream alerts: test sent")
  end
end

function plugin.diagnostics()
  return { { "Drops this run", tostring(drops) } }
end

function plugin.get_config()
  return { when_back = when_back }
end

function plugin.set_config(cfg)
  when_back = cfg.when_back ~= false
end

return plugin
