//! Lua plugins end to end: the sandbox, the two example plugins, check-plugin's report and the frozen plugin API.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use telescope_core::plugin::{load_config_tolerantly, Plugin};
use telescope_core::testing::scratch_dir;
use telescope_core::ui::SlintSource;
use telescope_core::Event;
use telescope_plugin_host::guest::{HOOKS, HOST_API};
use telescope_plugin_host::lua::GLOBALS;
use telescope_plugin_host::testing::{check, validate_shader, PluginTest};
use telescope_plugin_host::{OLDEST_PLUGIN_API, PLUGIN_API};

const EXAMPLES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/example-plugins");

fn example(name: &str) -> PathBuf {
    Path::new(EXAMPLES).join(name)
}

/// A plugin folder with this main.lua and plugin.json keys.
fn plugin(name: &str, main: &str, extra: Value) -> PathBuf {
    let folder = scratch_dir("lua").join(name);
    std::fs::create_dir(&folder).unwrap();
    let mut manifest = json!({"name": name, "api": 2});
    for (k, v) in extra.as_object().into_iter().flatten() {
        manifest[k] = v.clone();
    }
    std::fs::write(folder.join("plugin.json"), manifest.to_string()).unwrap();
    std::fs::write(folder.join("main.lua"), main).unwrap();
    folder
}

// ── The sandbox ─────────────────────────────────────────────────────────────

#[test]
fn a_plugin_can_reach_no_files_programs_or_network() {
    let folder = plugin("sealed", r#"
        local gone = { io = io, package = package, debug = debug, dofile = dofile, loadfile = loadfile, ["string.dump"] = string.dump,
                       ["os.execute"] = os.execute, ["os.remove"] = os.remove, ["os.rename"] = os.rename, ["os.exit"] = os.exit,
                       ["os.getenv"] = os.getenv, ["os.tmpname"] = os.tmpname, require_outside = pcall(require, "..x") or nil }
        local left = {}
        for name in pairs(gone) do left[#left + 1] = name end
        table.sort(left)
        return { left = function() return left end, now = function() return type(os.time()) end }
    "#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert_eq!(t.call("left", vec![]).unwrap(), json!({}), "an empty table: nothing dangerous is there");
    assert_eq!(t.call("now", vec![]).unwrap(), json!("number"));
}

#[test]
fn a_hook_that_never_ends_is_stopped() {
    let folder = plugin("loops", "return { setup = function() while true do end end }\n", json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    let started = std::time::Instant::now();
    let err = t.call("setup", vec![]).unwrap_err();
    assert!(err.contains("took too long") && started.elapsed() < std::time::Duration::from_secs(10), "{err}");
    assert_eq!(t.call_without_host("get_config", vec![]).unwrap_or_default(), Value::Null, "the plugin still answers afterwards");
}

#[test]
fn a_plugin_that_takes_too_much_memory_is_stopped() {
    let folder = plugin("hungry", "return { setup = function() local t = {} for i = 1, 1e9 do t[i] = string.rep('x', 4096) .. i end end }\n", json!({}));
    let err = PluginTest::open(&folder).unwrap().call("setup", vec![]).unwrap_err();
    assert!(err.contains("memory"), "{err}");
}

#[test]
fn host_methods_need_their_permission_and_a_host() {
    let folder = plugin("asks", r#"return {
        setup = function() return host.is_streaming() end,
        quit = function() host.quit_app() end,
        card = function() host.is_streaming() end,
        later = function() host.set_ui_props("card", { on = true }) end,
    }"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert_eq!(t.call("setup", vec![]).unwrap(), json!(false));
    assert!(t.call("quit", vec![]).unwrap_err().contains("\"app\""));
    assert!(t.call_without_host("card", vec![]).unwrap_err().contains("runs without the host"));
    t.call_without_host("later", vec![]).unwrap();
    assert!(t.host.calls.is_empty(), "waits for a hook with the host");
    t.call("setup", vec![]).unwrap();
    assert_eq!(t.take_calls(), ["set_ui_props card {\"on\":true}"]);
}

#[test]
fn a_set_config_that_raises_rejects_the_settings_and_the_rest_still_load() {
    let folder = plugin("picky", r#"
        local cfg = { size = 3, on = false }
        return {
          get_config = function() return cfg end,
          set_config = function(c)
            if c.size ~= nil and math.type(c.size) ~= "integer" then error("size has to be a whole number") end
            cfg = c
          end,
        }"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    assert!(t.set_config(json!({"size": "big"})).unwrap_err().contains("whole number"));
    load_config_tolerantly(&mut t.plugin, &json!({"size": 3, "on": false}), &json!({"size": "big", "on": true}));
    assert_eq!(t.config(), json!({"size": 3, "on": true}));
}

#[test]
fn a_plugin_emits_its_own_events_and_hears_them() {
    let folder = plugin("talker", r#"
        local heard = {}
        return {
          events = { ["talker.ping"] = function(e) heard[#heard + 1] = e.data.n end, stream_stopped = function() host.emit("talker.ping", { n = 2 }) end },
          heard = function() return heard end,
        }"#, json!({}));
    let mut t = PluginTest::open(&folder).unwrap();
    t.event(Event::StreamStopped);
    let sent = t.take_events();
    assert_eq!(sent, [Event::Custom { name: "talker.ping".into(), data: json!({"n": 2}) }]);
    t.event(sent[0].clone());
    assert_eq!(t.call("heard", vec![]).unwrap(), json!([2]));
}

// ── The examples ────────────────────────────────────────────────────────────

#[test]
fn black_and_white_has_a_card_a_setting_and_a_shader() {
    let mut t = PluginTest::open(example("black_and_white")).unwrap();
    t.setup();
    let card = t.card().unwrap();
    assert_eq!((card.region.as_str(), card.ui.component.as_str()), ("right", "BlackAndWhiteCard"));
    assert_eq!(card.ui.props, json!({"on": false, "strength": 100}));
    assert!(matches!(&card.ui.source, SlintSource::File(path) if path.ends_with("card.slint")));
    assert!(t.shader().is_none(), "off: frames pass untouched");

    t.ui("card", "toggled", vec![json!(true)]);
    assert_eq!(t.take_calls(), ["refresh_frame_step", "schedule_save"]);
    let step = t.shader().unwrap();
    assert_eq!(step.uniforms, json!({"strength": 1.0}));
    validate_shader(&step, "shaders/grey.wgsl").unwrap();
    t.ui("card", "strength-moved", vec![json!(42.4)]);
    assert_eq!(t.take_calls(), ["refresh_frame_step", "schedule_save"]);
    assert_eq!(t.shader().unwrap().uniforms, json!({"strength": 0.42}));
    assert_eq!(t.config(), json!({"on": true, "strength": 42}));

    t.set_config(json!({"on": false, "strength": "lots"})).unwrap();
    assert_eq!(t.config(), json!({"on": false, "strength": 100}), "a value from an older version or a hand edit falls back");
    t.event(Event::StreamStopped);
    let calls = t.take_calls();
    assert_eq!(calls.len(), 2, "done once a hook has the host: {calls:?}");
    let props: Value = serde_json::from_str(calls[0].strip_prefix("set_ui_props card ").unwrap()).unwrap();
    assert_eq!((props, calls[1].as_str()), (json!({"on": false, "strength": 100}), "refresh_frame_step"));
}

#[test]
fn stream_alerts_follows_events_and_calls_the_host() {
    let mut t = PluginTest::open(example("stream_alerts")).unwrap();
    t.start_stream("phone");
    assert!(t.take_calls().is_empty());
    t.event(Event::StreamLost { source_id: "phone".into() });
    t.event(Event::StreamLost { source_id: "phone".into() });
    assert_eq!(t.take_calls(), ["send_notification Stream dropped"], "once per drop");
    t.event(Event::StreamConnected { source_id: "phone".into() });
    t.event(Event::StreamConnected { source_id: "phone".into() });
    assert_eq!(t.take_calls(), ["send_notification Stream is back"]);
    assert_eq!(t.plugin.diagnostics(), [("Drops this run".to_string(), "1".to_string())]);
    let actions = t.plugin.actions();
    assert_eq!(actions[0].id, "stream_alerts.test");
    assert_eq!(t.plugin.run_action(&mut t.host, "stream_alerts.test", &json!({})).as_deref(), Some("Stream alerts: test sent"));
    t.set_config(json!({"when_back": false})).unwrap();
    t.event(Event::StreamLost { source_id: "phone".into() });
    t.event(Event::StreamConnected { source_id: "phone".into() });
    assert_eq!(t.take_calls(), ["send_notification Stream alerts", "send_notification Stream dropped"]);
}

#[test]
fn stream_alerts_needs_its_permission() {
    let folder = scratch_dir("lua").join("stream_alerts");
    std::fs::create_dir(&folder).unwrap();
    std::fs::copy(example("stream_alerts").join("main.lua"), folder.join("main.lua")).unwrap();
    std::fs::write(folder.join("plugin.json"), json!({"name": "stream_alerts", "api": 2}).to_string()).unwrap();
    let mut t = PluginTest::open(&folder).unwrap();
    let lost = serde_json::to_value(Event::StreamLost { source_id: "phone".into() }).unwrap();
    assert!(t.call("on_event", vec![lost]).unwrap_err().contains("\"notifications\""));
}

// ── check-plugin (test_check_plugin.py) ─────────────────────────────────────

#[test]
fn the_example_plugins_pass() {
    for name in ["black_and_white", "stream_alerts"] {
        let report = check(&example(name));
        assert_eq!(report.problems(), 0, "{}", report.text());
        assert!(report.text().ends_with("All good.\n"));
    }
}

#[test]
fn a_broken_plugin_gets_each_problem_with_where_it_is() {
    let folder = plugin("bad_one", "local plugin = {}\n\
        function plugin.frame_step() return { shader = 'fx.wgsl' } end\n\
        function plugin.card() return { file = 'card.slint', component = 'Card' } end\n\
        function plugin.set_config(cfg) end\n\
        function plugin.on_stream_start(stream)\n  error('no lens')\nend\n\
        return plugin\n", json!({}));
    std::fs::write(folder.join("fx.wgsl"), "fn effect(uv: vec2<f32>) -> vec4<f32> {\n    return sample(uv) * nope;\n}\n").unwrap();
    let report = check(&folder);
    let text = report.text();
    assert_eq!(report.problems(), 3, "{text}");
    assert!(text.contains("3 problems to fix"), "{text}");
    assert!(text.contains("fx.wgsl:2"), "the shader's error points into its file: {text}");
    assert!(text.contains("main.lua:6: no lens"), "{text}");
    assert!(text.contains("the plugin has no card.slint"), "{text}");
}

#[test]
fn a_plugin_that_wont_load_says_why() {
    let folder = plugin("broken", "require('not_a_module_anywhere')\nreturn {}\n", json!({}));
    let report = check(&folder);
    assert!(report.problems() == 1 && report.text().contains("not_a_module_anywhere"), "{}", report.text());
}

#[test]
fn repo_is_checked() {
    let cases = [
        (json!({"repo": "github.com/me/extra"}), "[FAIL] plugin.json"),
        (json!({"repo": "https://github.com/me/extra", "version": "1.0"}), "Telescope can tell people when a new release"),
        (json!({"repo": "https://github.com/me/extra"}), "Give it a \"version\""),
        (json!({"repo": "https://codeberg.org/me/extra"}), "only follow https://github.com/owner/repo"),
        (json!({}), ""),
    ];
    for (extra, expect) in cases {
        let report = check(&plugin("extra", "return {}\n", extra.clone()));
        let text = report.text();
        let about_repo = report.notes.len() + report.steps.iter().filter(|(w, r)| w == "plugin.json" && r.is_err()).count();
        assert!(text.contains(expect) && (about_repo > 0) == !expect.is_empty(), "{extra}: {text}");
    }
}

// ── The plugin API, frozen (test_plugin_api.py) ─────────────────────────────

fn current_api() -> String {
    let mut lines = vec![format!("PLUGIN_API = {PLUGIN_API}"), format!("OLDEST_PLUGIN_API = {OLDEST_PLUGIN_API}")];
    for m in HOST_API {
        let permission = m.permission.map(|p| format!(" [{p}]")).unwrap_or_default();
        let later = if m.deferrable { " [waits for the host]" } else { "" };
        lines.push(format!("host.{}({}){permission}{later}", m.name, m.params.join(", ")));
    }
    for (hook, args, with_host) in HOOKS {
        lines.push(format!("plugin.{hook}({args}){}", if *with_host { "" } else { " [no host]" }));
    }
    lines.extend(GLOBALS.iter().map(|g| format!("global {g}")));
    lines.extend(Event::NAMES.iter().map(|e| format!("event {e}")));
    lines.extend(telescope_plugin_host::manifest::PERMISSIONS.iter().map(|(p, _)| format!("permission {p}")));
    lines.push(format!("shader entry {}", telescope_core::shader::ENTRY));
    lines.push(format!("shader params {}", telescope_core::shader::PARAMS));
    lines.sort();
    lines.join("\n") + "\n"
}

#[test]
fn the_plugin_api_only_changes_on_purpose() {
    let frozen = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugin_api.txt");
    let now = current_api();
    if std::env::var_os("UPDATE_PLUGIN_API").is_some() {
        std::fs::write(&frozen, &now).unwrap();
    }
    let was = std::fs::read_to_string(&frozen).unwrap_or_default();
    if now == was {
        return;
    }
    let (old, new): (Vec<&str>, Vec<&str>) = (was.lines().collect(), now.lines().collect());
    let gone: Vec<&&str> = old.iter().filter(|l| !new.contains(l)).collect();
    let added: Vec<&&str> = new.iter().filter(|l| !old.contains(l)).collect();
    let advice = if gone.is_empty() {
        "Only additions, which plugins don't mind. Write the new list with UPDATE_PLUGIN_API=1."
    } else {
        "Something plugins may use changed or went away, which breaks the ones using it. Keep the old one working next to the new one if you \
         can. Only if you can't, raise PLUGIN_API in manifest.rs. Then write the new list with UPDATE_PLUGIN_API=1."
    };
    panic!("The plugin API changed.\n{}{}\n\n{advice}", gone.iter().map(|l| format!("\n  - {l}")).collect::<String>(),
           added.iter().map(|l| format!("\n  + {l}")).collect::<String>());
}
