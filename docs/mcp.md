# MCP server

The MCP server lets an AI app like Claude Desktop look at Telescope and change its settings. Someone whose picture lags can switch it on and ask the model what's going on. Someone else can describe the look they want and let the model set the colour grading, or ask where a setting is.

It's off until you switch it on, and it opens no network port. The AI app starts a small `telescope mcp` command, which talks to the Telescope that's already running on your computer, and only to yours. Telescope sends nothing to a model by itself. The model only sees what your AI app asks for and shows you.

## Switching it on

1. Open **Settings**, then the **Plugins** tab
2. Switch on **MCP server** (it's under Extras) and click **Restart now**
3. Add Telescope to your AI app, see below

Telescope shows a notification when the first AI app connects. Switching the plugin off again turns every connection away.

Once it's on there's an **MCP** tab in Settings and an **MCP** button in the top bar. The button's dot shows whether an AI app is connected right now and lights up for a moment when the AI looks at something or changes something. Its left side opens the MCP tab. Its right side undoes what the AI changed, for every AI app, and is greyed out when there's nothing to undo.

The MCP tab has:

- the AI apps connected now, by the name each one gave
- **Access**: **Read only** or **Read and change** (the default). Read only turns away **set**, **revert** and **run_action**, which includes Auto colour grading. Reading still works. The undo button is yours, so it works either way
- the entry to put in your AI app's MCP settings, with a copy button
- the last 30 things the AI did, newest first, with refused ones marked. It lives in memory only and is gone when Telescope quits

When the AI changes a setting whose control is on screen, that control gets an outline for a second and a half, and it does the same when the AI reads a few named settings. Controls that are folded away or on another tab are skipped. Nothing opens or scrolls.

## Adding it to an AI app

For Claude Desktop, open its settings, then **Developer** and **Edit Config**, and add Telescope to `mcpServers`:

```json
{
  "mcpServers": {
    "telescope": { "command": "telescope", "args": ["mcp"] }
  }
}
```

Use the full path to the program if `telescope` isn't on your PATH. Other AI apps that run MCP servers from a command take the same two things, the command and `mcp` as its argument. If Telescope isn't running or the plugin is off when the AI app starts, the command still starts and tells the model why nothing works yet. It connects as soon as Telescope is up, with no restart of the AI app.

## What it can do

Reading:

- **status** shows whether a phone is streaming, the sources, which plugins are on and how many AI apps are connected
- **describe** and **get** list the settings the model can change, each with its range, default, what it does and where it is now
- **diagnostics** gives the same text as **Copy diagnostics**, with the same personal details left out
- **measure** looks at the picture for about a second and gives it as numbers (the face's colour and brightness, a colour cast, how dark, bright and noisy it is). It needs a stream running and Colour grading on, and it's the same measuring Auto colour grading uses

Changing:

- **set** changes several settings at once and the camera never shows a half-way step. Every value is checked first, and when one is out of range or not a setting nothing changes and the model gets told which ones were wrong
- **revert** puts back what **set** and **run_action** changed, including settings that moved along with it and what an action switched that isn't a listed setting (Privacy blur's switch, the microphone's gain, the background), newest change first. Each AI app only undoes its own changes. Anything you or another AI app moved afterwards stays the way it is, even when you landed on the same value again, and so does a setting that putting the other one back would have moved along. Telescope keeps the last 200 steps, and keeps them when it restarts. After a restart only the undo button reaches them, since no AI app owns them any more
- A step for a camera that isn't shown, or a plugin that's switched off, isn't lost: undo says it was kept, and it goes back the next time you undo while that camera is shown or the plugin is on
- **actions** and **run_action** run the same actions keyboard shortcuts run. The settings an action moves are undone by **revert** too, and its answer says when it moved none (so there is nothing to undo). Actions that change something that isn't saved, like a mute or Blur now, can't be put back, and **revert** names them in its answer (the undo button's line in the log counts them)

Changes show in the camera straight away, which matters in a live call, so the model is told to make small steps. An AI app can have 4 calls running at once (12 across all of them), and up to 8 can be connected. Starting or stopping the stream, switching the phone's camera off and picking another phone are left to you because they can drop a call. `telescope --action` is there if you want them from a command. A message longer than 1 MiB is turned away.

Settings are declared by plugins. Colour grading and Transforms declare theirs so far, and more plugins will follow. A plugin that's switched off doesn't show up in **describe**.

- **stats** gives the numbers the Stats panel shows (frame rates in and out, drops with their reasons, bitrate, decode and frame step times) over the last minute, each as min, average and max with the worst moments and how long ago they were. `level: nerd` adds the stage times, the balancer's moves and the app's CPU and memory
- **watch** collects the same numbers for 5 to 120 seconds while you reproduce a problem, then answers

Both read the numbers core already keeps, so they work with the Stats panel switched off. They need a stream running.

**Not here yet:** frames from the camera are never sent.

## Trying it

`telescope mcp` speaks the protocol on stdin and stdout. While Telescope runs you can try it by hand from a terminal:

```
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | telescope mcp
```

A dev desktop (`telescope-rs dev`) has its own socket. Start the command with the same `TELESCOPE_RS_PROFILE` and it talks to that one.
