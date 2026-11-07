# MCP server

The MCP server lets an AI app like Claude Desktop look at Telescope and change its settings. Someone whose picture lags can switch it on and ask the model what's going on. Someone else can describe the look they want and let the model set the colour grading, or ask where a setting is.

It's off until you switch it on, and it opens no network port. The AI app starts a small `telescope mcp` command, which talks to the Telescope that's already running on your computer, and only to yours. Telescope sends nothing to a model by itself. The model only sees what your AI app asks for and shows you.

## Switching it on

1. Open **Settings**, then the **Plugins** tab
2. Switch on **MCP server** (it's under Extras) and click **Restart now**
3. Add Telescope to your AI app, see below

Telescope shows a notification when the first AI app connects. Switching the plugin off again turns every connection away.

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
- **revert** puts back what **set** changed, including settings that moved along with it. Each AI app only undoes its own changes, and a setting you moved by hand afterwards stays the way you left it
- **actions** and **run_action** run the same actions keyboard shortcuts run

Changes show in the camera straight away, which matters in a live call, so the model is told to make small steps. Starting or stopping the stream, switching the phone's camera off and picking another phone are left to you because they can drop a call. `telescope --action` is there if you want them from a command.

Settings are declared by plugins. Colour grading and Transforms declare theirs so far, and more plugins will follow. A plugin that's switched off doesn't show up in **describe**.

**Not here yet:** the Stats numbers (**stats** and **watch**) answer that they aren't available in this build. Frames from the camera are never sent.

## Trying it

`telescope mcp` speaks the protocol on stdin and stdout. While Telescope runs you can try it by hand from a terminal:

```
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | telescope mcp
```

A dev desktop (`telescope-rs dev`) has its own socket. Start the command with the same `TELESCOPE_RS_PROFILE` and it talks to that one.
