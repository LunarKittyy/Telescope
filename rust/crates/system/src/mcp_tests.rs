use super::*;
use telescope_core::settings::SettingSpec;
use telescope_core::testing::FakeHost;
use telescope_core::ui::ActionSpec;

fn spec_list() -> Vec<SettingSpec> {
    vec![
        SettingSpec::switch("on", "Whether it is applied", true),
        SettingSpec::number("contrast", "How far tones spread", 0.0, 2.0, 2, 1.0),
        SettingSpec::number("masters.gain", "Highlights", 0.0, 2.0, 2, 1.0),
        SettingSpec::choice("mode", "How", &[("fast", "Quick"), ("fine", "Careful")], "fast"),
    ]
}

struct Rig {
    plugin: McpPlugin,
    host: FakeHost,
    handle: McpHandle,
}

impl Rig {
    fn new() -> Rig {
        let mut host = FakeHost::new();
        host.settings = vec![PluginSettings { name: "grading".into(), title: "Colour grading".into(), settings: spec_list() }];
        host.plugin_configs.insert("grading".into(), json!({"on": true, "contrast": 1.0, "masters": {"gain": 1.0, "lift": 0.0}, "lut": "keep.cube"}));
        host.plugins = vec!["grading".into(), "mcp".into()];
        host.streaming = true;
        host.focused = Some("phone".into());
        let handle = McpHandle::default();
        let mut plugin = McpPlugin::new(handle.clone());
        plugin.setup(&mut host);
        Rig { plugin, host, handle }
    }

    fn deliver(&mut self) {
        for (token, data) in self.host.take_posted() {
            self.plugin.on_event(&mut self.host, &Event::Posted { owner: NAME.into(), token, data });
        }
    }

    /// A tool call from a connection's thread, answered by the plugin on this one. `between` runs while it waits, for events other plugins would send.
    fn ask_with(&mut self, tool: &str, args: Value, mut between: impl FnMut(&mut Rig)) -> Reply {
        let (handle, tool) = (self.handle.clone(), tool.to_string());
        let asked = std::thread::spawn(move || handle.call(&tool, args));
        while !asked.is_finished() {
            self.deliver();
            between(self);
            std::thread::sleep(Duration::from_millis(1));
        }
        asked.join().unwrap()
    }

    fn ask(&mut self, tool: &str, args: Value) -> Reply {
        self.ask_with(tool, args, |_| {})
    }

    fn config(&self) -> Value {
        self.host.plugin_configs["grading"].clone()
    }
}

fn never(_: &str, _: Value) -> Reply {
    panic!("no tool should run")
}

// ── The protocol ─────────────────────────────────────────────────────

#[test]
fn initialize_agrees_a_known_protocol_version_and_names_the_server() {
    let ask = |version: &str| handle_message(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": version}}), &never).unwrap();
    let known = ask("2024-11-05");
    assert_eq!(known["id"], json!(1));
    assert_eq!(known["result"]["protocolVersion"], json!("2024-11-05"));
    assert_eq!(known["result"]["serverInfo"]["name"], json!("telescope"));
    assert!(known["result"]["capabilities"]["tools"].is_object());
    assert!(known["result"]["instructions"].as_str().unwrap().contains("status"));
    assert_eq!(ask("1999-01-01")["result"]["protocolVersion"], json!(LATEST), "an unknown version gets the newest one this speaks");
}

#[test]
fn notifications_get_no_answer_and_ping_gets_an_empty_one() {
    assert_eq!(handle_message(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &never), None);
    assert_eq!(handle_message(&json!({"jsonrpc": "2.0", "id": 7, "method": "ping"}), &never).unwrap()["result"], json!({}));
}

#[test]
fn tools_list_names_every_tool_with_a_schema_and_says_which_only_read() {
    let answer = handle_message(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &never).unwrap();
    let list = answer["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = list.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["status", "stats", "watch", "diagnostics", "measure", "describe", "get", "set", "revert", "actions", "run_action"]);
    for t in list {
        assert_eq!(t["inputSchema"]["type"], json!("object"), "{t}");
        assert!(!t["description"].as_str().unwrap().is_empty());
    }
    let read_only = |name: &str| list.iter().find(|t| t["name"] == name).unwrap()["annotations"]["readOnlyHint"].clone();
    assert_eq!((read_only("describe"), read_only("set"), read_only("run_action")), (json!(true), json!(false), json!(false)));
}

#[test]
fn tools_call_passes_the_arguments_on_and_flags_a_failure() {
    let seen = Mutex::new(Vec::new());
    let call = |name: &str, args: Value| -> Reply {
        seen.lock().unwrap().push((name.to_string(), args));
        if name == "set" { Err("Nothing was changed.".into()) } else { Ok(json!({"fine": true})) }
    };
    let ask = |name: &str, arguments: Value| handle_message(&json!({"jsonrpc": "2.0", "id": "x", "method": "tools/call", "params": {"name": name, "arguments": arguments}}), &call).unwrap();
    let ok = ask("get", json!({"ids": ["a.b"]}));
    assert_eq!(ok["id"], json!("x"));
    assert_eq!(ok["result"]["isError"], json!(false));
    assert!(ok["result"]["content"][0]["text"].as_str().unwrap().contains("\"fine\": true"));
    let bad = ask("set", json!({"changes": {}}));
    assert_eq!((bad["result"]["isError"].clone(), bad["result"]["content"][0]["text"].clone()), (json!(true), json!("Nothing was changed.")));
    assert_eq!(seen.lock().unwrap()[0], ("get".to_string(), json!({"ids": ["a.b"]})));
}

#[test]
fn an_unknown_tool_or_method_is_a_protocol_error_and_bad_arguments_a_tool_error() {
    let ask = |message: Value| handle_message(&message, &never).unwrap();
    assert_eq!(ask(json!({"id": 1, "method": "tools/call", "params": {"name": "format_disk"}}))["error"]["code"], json!(-32602));
    assert_eq!(ask(json!({"id": 2, "method": "resources/list"}))["error"]["code"], json!(-32601));
    assert_eq!(ask(json!({"id": 3, "params": {}}))["error"]["code"], json!(-32600));
    let bad = ask(json!({"id": 4, "method": "tools/call", "params": {"name": "get", "arguments": [1]}}));
    assert_eq!(bad["result"]["isError"], json!(true));
}

/// A writer that hands every message to a channel, so a test waits for answers instead of polling.
struct Lines(Sender<String>);

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(String::from_utf8_lossy(buf).trim_end().to_string());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_connection_answers_line_by_line_and_a_slow_tool_does_not_hold_up_the_next_request() {
    let (release, wait) = channel::<()>();
    let wait = Mutex::new(wait);
    let call: Arc<Caller> = Arc::new(move |name, _| {
        if name == "measure" {
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(5));
        }
        Ok(json!({"tool": name}))
    });
    let (tx, rx) = channel();
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"measure\"}}\n",
        "not json\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    );
    serve_connection(std::io::Cursor::new(input), Arc::new(Mutex::new(Lines(tx))), call);
    let first: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    let second: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(first["error"]["code"], json!(-32700), "bad JSON is answered");
    assert_eq!(second["id"], json!(2), "the ping was answered while measure was still running");
    release.send(()).unwrap();
    let last: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(last["id"], json!(1));
    assert!(last["result"]["content"][0]["text"].as_str().unwrap().contains("measure"));
}

// ── Off by default, and what a connection finds ─────────────────────

#[test]
fn while_the_plugin_is_off_a_connection_is_turned_away_and_a_call_says_so() {
    let handle = McpHandle::default();
    assert!(!handle.is_on());
    let why = handle.open_client().err().unwrap();
    assert!(why.contains("switched off") && why.contains("Plugins"), "{why}");
    assert!(accept(&handle).is_err());
    assert_eq!(handle.call("status", json!({})).unwrap_err(), off());
}

#[test]
fn a_plugin_that_is_switched_off_again_lets_go_of_what_waits() {
    let mut r = Rig::new();
    let handle = r.handle.clone();
    let waiting = std::thread::spawn(move || handle.call("measure", json!({})));
    while r.host.take_posted().is_empty() {
        std::thread::sleep(Duration::from_millis(1));
    }
    r.plugin.shutdown(&mut r.host);
    assert_eq!(waiting.join().unwrap().unwrap_err(), off());
    assert!(!r.handle.is_on());
}

#[test]
fn the_first_connection_is_announced_and_counted_until_it_goes() {
    let mut r = Rig::new();
    let client = r.handle.open_client().unwrap();
    assert_eq!(r.handle.clients(), 1);
    r.deliver();
    assert_eq!(r.host.notifications.len(), 1, "the person is told an AI app connected");
    let second = r.handle.open_client().unwrap();
    r.deliver();
    assert_eq!(r.host.notifications.len(), 1, "only the first one is announced");
    drop((client, second));
    assert_eq!(r.handle.clients(), 0);
    let status = r.ask("status", json!({})).unwrap();
    assert_eq!(status["mcp"]["clients"], json!(0));
}

#[test]
fn the_plugin_is_optional_so_it_starts_off() {
    let info = McpPlugin::new(McpHandle::default()).info();
    assert!(info.optional && !info.required);
    assert!(McpPlugin::new(McpHandle::default()).settings().is_empty());
}

// ── Reading ──────────────────────────────────────────────────────────

#[test]
fn status_says_what_is_going_on_without_any_address_or_token() {
    let mut r = Rig::new();
    r.host.canvas = (Some(1920), Some(1080));
    let status = r.ask("status", json!({})).unwrap();
    assert_eq!(status["streaming"]["streaming"], json!(true));
    assert_eq!(status["streaming"]["canvas"], json!({"width": 1920, "height": 1080}));
    assert_eq!(status["focused_source"], json!("phone"));
    assert!(status["plugins_running"].as_array().unwrap().iter().any(|p| p["name"] == "grading"));
    assert_eq!(status["stats"], json!("not available in this build"));
}

#[test]
fn describe_lists_every_setting_with_its_range_and_where_it_is_now() {
    let mut r = Rig::new();
    let all = r.ask("describe", json!({})).unwrap();
    let settings = all["plugins"][0]["settings"].as_array().unwrap();
    assert_eq!(settings.len(), 4);
    let gain = settings.iter().find(|s| s["id"] == "grading.masters.gain").unwrap();
    assert_eq!((gain["min"].clone(), gain["max"].clone(), gain["value"].clone(), gain["about"].clone()), (json!(0.0), json!(2.0), json!(1.0), json!("Highlights")));
    let one = r.ask("describe", json!({"plugin": "grading"})).unwrap();
    assert_eq!(one, all);
    let missing = r.ask("describe", json!({"plugin": "background"})).unwrap_err();
    assert!(missing.contains("switched off"), "{missing}");
}

#[test]
fn get_reads_by_id_and_names_what_it_could_not_find() {
    let mut r = Rig::new();
    let some = r.ask("get", json!({"ids": ["grading.contrast", "grading.masters.gain"]})).unwrap();
    assert_eq!(some["values"], json!({"grading.contrast": 1.0, "grading.masters.gain": 1.0}));
    assert_eq!(r.ask("get", json!({})).unwrap()["values"].as_object().unwrap().len(), 4);
    let err = r.ask("get", json!({"ids": ["grading.contras"]})).unwrap_err();
    assert!(err.contains("Did you mean grading.contrast"), "{err}");
    assert!(r.ask("get", json!({"ids": "grading.on"})).unwrap_err().contains("list"));
}

#[test]
fn diagnostics_gives_the_copy_diagnostics_text() {
    let mut r = Rig::new();
    r.host.report = "Telescope 5.1\nStreaming: yes".into();
    assert_eq!(r.ask("diagnostics", json!({})).unwrap()["text"], json!("Telescope 5.1\nStreaming: yes"));
}

#[test]
fn stats_and_watch_say_they_are_not_here_yet() {
    let mut r = Rig::new();
    for tool in ["stats", "watch"] {
        let why = r.ask(tool, json!({"window_seconds": 30})).unwrap_err();
        assert!(why.contains("not available in this build"), "{why}");
    }
}

// ── Setting ──────────────────────────────────────────────────────────

#[test]
fn set_changes_several_settings_at_once_through_the_plugins_own_preset_and_keeps_the_rest() {
    let mut r = Rig::new();
    let done = r.ask("set", json!({"changes": {"grading.contrast": 1.4, "grading.masters.gain": 0.8, "grading.on": false}})).unwrap();
    let changed = done["changed"].as_array().unwrap();
    assert_eq!(changed.len(), 3);
    let contrast = changed.iter().find(|c| c["id"] == "grading.contrast").unwrap();
    assert_eq!((contrast["was"].clone(), contrast["now"].clone()), (json!(1.0), json!(1.4)));
    assert_eq!(r.config(), json!({"on": false, "contrast": 1.4, "masters": {"gain": 0.8, "lift": 0.0}, "lut": "keep.cube"}), "what wasn't named is as it was");
    assert_eq!(r.host.calls.iter().filter(|c| c.starts_with("apply_preset grading")).count(), 1, "one write for the whole change");
    assert!(r.host.calls.iter().any(|c| c == "schedule_save"), "and it is saved");
}

#[test]
fn a_bad_value_changes_nothing_and_every_problem_is_listed() {
    let mut r = Rig::new();
    let before = r.config();
    let err = r.ask("set", json!({"changes": {"grading.contrast": 5, "grading.on": "yes", "grading.nope": 1, "grading.mode": "slow", "other.thing": 1}})).unwrap_err();
    assert!(err.starts_with("Nothing was changed."), "{err}");
    for part in ["out of range", "true or false", "grading.nope", "fast | fine", "other.thing"] {
        assert!(err.contains(part), "{part} in {err}");
    }
    assert_eq!(r.config(), before);
    assert!(!r.host.calls.iter().any(|c| c.starts_with("apply_preset")), "not even the good ones went in");
}

#[test]
fn a_value_that_is_fine_for_one_setting_does_not_let_a_bad_one_through_beside_it() {
    let mut r = Rig::new();
    let before = r.config();
    let err = r.ask("set", json!({"changes": {"grading.contrast": 1.2, "grading.masters.gain": 2.5}})).unwrap_err();
    assert!(err.contains("masters.gain") && err.contains("out of range"), "{err}");
    assert_eq!(r.config(), before);
    for bad in [json!({}), json!({"changes": {}}), json!({"changes": [1]}), json!({"changes": "x"})] {
        assert!(r.ask("set", bad).unwrap_err().contains("changes has to be"));
    }
}

#[test]
fn revert_puts_back_what_set_changed_and_leaves_what_the_person_moved_since() {
    let mut r = Rig::new();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4, "grading.masters.gain": 0.8}})).unwrap();
    r.ask("set", json!({"changes": {"grading.contrast": 1.6}})).unwrap();
    // the person drags gain by hand
    r.host.plugin_configs.get_mut("grading").unwrap()["masters"]["gain"] = json!(0.3);
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"].as_array().unwrap().len(), 1);
    assert_eq!(back["reverted"][0], json!({"id": "grading.contrast", "back_to": 1.0}), "the first value this session, not the middle one");
    assert_eq!(back["left"][0]["id"], json!("grading.masters.gain"));
    assert!(back["left"][0]["why"].as_str().unwrap().contains("by hand"));
    assert_eq!(r.config()["contrast"], json!(1.0));
    assert_eq!(r.config()["masters"]["gain"], json!(0.3), "the person's edit stays");
    let again = r.ask("revert", json!({})).unwrap();
    assert_eq!(again["reverted"], json!([]));
}

#[test]
fn revert_leaves_a_setting_alone_when_another_camera_is_shown() {
    let mut r = Rig::new();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    r.host.focused = Some("other phone".into());
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([]));
    assert!(back["left"][0]["why"].as_str().unwrap().contains("different camera"));
    assert_eq!(r.config()["contrast"], json!(1.4));
}

// ── Actions ──────────────────────────────────────────────────────────

fn with_actions(r: &mut Rig) {
    let action = |id: &str| ActionSpec { id: id.into(), label: id.into(), group: "Grading".into(), params: vec![], repeats: false, on_release: false, current: None, keywords: String::new() };
    r.host.actions = vec![("grading".into(), action("grading.reset"))];
}

#[test]
fn actions_list_what_can_run_and_leave_out_what_can_drop_a_call() {
    let mut r = Rig::new();
    with_actions(&mut r);
    let listed = r.ask("actions", json!({})).unwrap();
    let ids: Vec<&str> = listed["actions"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["grading.reset"], "the stream and camera controls are the person's");
}

#[test]
fn run_action_runs_one_and_refuses_the_ones_left_to_the_person() {
    let mut r = Rig::new();
    with_actions(&mut r);
    assert_eq!(r.ask("run_action", json!({"action": "grading.reset"})).unwrap(), json!({"said": "grading.reset ran"}));
    for id in OUT_OF_REACH {
        let why = r.ask("run_action", json!({"action": id})).unwrap_err();
        assert!(why.contains("left to the person"), "{why}");
    }
    assert!(!r.host.calls.iter().any(|c| c.contains("start_stream") || c.contains("stop")));
    assert!(r.ask("run_action", json!({"action": "nope"})).unwrap_err().contains("no action called"));
    assert!(r.ask("run_action", json!({})).unwrap_err().contains("action has to be"));
    assert!(r.ask("run_action", json!({"action": "grading.reset", "params": 3})).unwrap_err().contains("params has to be"));
}

#[test]
fn an_actions_params_are_checked_so_a_typo_does_not_run_the_default() {
    let mut r = Rig::new();
    let slider = json!([
        {"name": "op", "kind": "choice", "default": "up", "choices": [["up", "Increase by"], ["set", "Set to"]]},
        {"name": "amount", "kind": "number", "default": 5.0, "minimum": 0.0, "maximum": 10.0, "shown_if": ["op", ["up"]]},
        {"name": "value", "kind": "number", "default": null, "minimum": -100.0, "maximum": 100.0, "shown_if": ["op", ["set"]]},
    ]);
    r.host.actions = vec![("grading".into(), ActionSpec { id: "grading.tint".into(), label: "Tint".into(), group: "Grading".into(), params: slider.as_array().unwrap().clone(),
                                                          repeats: true, on_release: false, current: Some(0.0), keywords: String::new() })];
    let bad = |r: &mut Rig, params: Value| r.ask("run_action", json!({"action": "grading.tint", "params": params})).unwrap_err();
    assert!(bad(&mut r, json!({"ammount": 3})).contains("has no setting \"ammount\""));
    assert!(bad(&mut r, json!({"op": "sideways"})).contains("up | set"));
    assert!(bad(&mut r, json!({"amount": 50})).contains("out of range"));
    assert!(bad(&mut r, json!({"amount": "lots"})).contains("has to be a number"));
    assert!(bad(&mut r, json!({"op": "set"})).contains("needs value"));
    assert!(r.host.calls.iter().all(|c| !c.starts_with("run_action")), "nothing ran");
    r.ask("run_action", json!({"action": "grading.tint", "params": {"op": "set", "value": "-20"}})).unwrap();
    let ran = r.host.calls.iter().find(|c| c.starts_with("run_action")).unwrap().clone();
    assert!(ran.contains("-20") && ran.contains("\"op\":\"set\""), "{ran}");
}

// ── Measuring is a job ───────────────────────────────────────────────

#[test]
fn measure_asks_the_bus_and_answers_when_the_picture_is_measured() {
    let mut r = Rig::new();
    let mut answered = false;
    let reply = r.ask_with("measure", json!({}), |r| {
        let asked = r.host.events.iter().find_map(|e| match e {
            Event::PictureMeasureRequested { token } => Some(token.clone()),
            _ => None,
        });
        if let (Some(token), false) = (asked, answered) {
            answered = true;
            assert!(r.host.is_scheduled(&format!("job:{token}")), "a job nobody answers expires");
            r.plugin.on_event(&mut r.host, &Event::PictureMeasured { token: token.clone(), ok: true, message: String::new(), data: json!({"frames": 12}) });
            assert!(!r.host.is_scheduled(&format!("job:{token}")));
        }
    });
    assert_eq!(reply.unwrap(), json!({"frames": 12}));
}

#[test]
fn a_measure_nobody_can_do_times_out_with_a_reason_and_a_refusal_comes_through() {
    let mut r = Rig::new();
    let mut fired = false;
    let nobody = r.ask_with("measure", json!({}), |r| {
        if let (true, false) = (!r.host.scheduled.is_empty(), fired) {
            fired = true;
            let token = r.host.scheduled[0].1.clone();
            r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token });
        }
    });
    assert!(nobody.unwrap_err().contains("Colour grading switched on"));
    let mut told = false;
    let refused = r.ask_with("measure", json!({}), |r| {
        let token = r.host.events.iter().rev().find_map(|e| match e {
            Event::PictureMeasureRequested { token } if token != "measure1" => Some(token.clone()),
            _ => None,
        });
        if let (Some(token), false) = (token, told) {
            told = true;
            r.plugin.on_event(&mut r.host, &Event::PictureMeasured { token, ok: false, message: "No stream is running".into(), data: Value::Null });
        }
    });
    assert_eq!(refused.unwrap_err(), "No stream is running");
}

#[test]
fn an_answer_for_somebody_elses_measuring_is_ignored() {
    let mut r = Rig::new();
    r.plugin.on_event(&mut r.host, &Event::PictureMeasured { token: "other".into(), ok: true, message: String::new(), data: json!({}) });
    assert!(r.plugin.jobs.is_empty());
}
