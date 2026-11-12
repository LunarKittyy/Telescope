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
    fn ask_with(&mut self, tool: &str, args: Value, between: impl FnMut(&mut Rig)) -> Reply {
        self.ask_as_with(0, tool, args, between)
    }

    fn ask_as_with(&mut self, session: u64, tool: &str, args: Value, mut between: impl FnMut(&mut Rig)) -> Reply {
        let (handle, tool) = (self.handle.clone(), tool.to_string());
        let asked = std::thread::spawn(move || handle.call_as(session, &tool, args));
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

    fn ask_as(&mut self, session: u64, tool: &str, args: Value) -> Reply {
        self.ask_as_with(session, tool, args, |_| {})
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
    assert_eq!(ask(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "format_disk"}}))["error"]["code"], json!(-32602));
    assert_eq!(ask(json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list"}))["error"]["code"], json!(-32601));
    assert_eq!(ask(json!({"jsonrpc": "2.0", "id": 3, "params": {}}))["error"]["code"], json!(-32600));
    let bad = ask(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "get", "arguments": [1]}}));
    assert_eq!(bad["result"]["isError"], json!(true));
}

#[test]
fn a_message_that_is_not_a_valid_request_is_refused_and_never_runs_a_tool() {
    let ask = |message: Value| handle_message(&message, &never);
    let call = |extra: Value| {
        let mut m = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "set", "arguments": {"changes": {"a.b": 1}}}});
        for (k, v) in extra.as_object().unwrap() {
            if v.is_null() { m.as_object_mut().unwrap().remove(k); } else { m[k] = v.clone(); }
        }
        m
    };
    for bad in [call(json!({"jsonrpc": null})), call(json!({"jsonrpc": "1.0"})), call(json!({"id": true})), call(json!({"id": {"a": 1}})),
                call(json!({"id": 1.5})), json!({"jsonrpc": "2.0", "id": null, "method": "ping"}), call(json!({"id": [1]})), json!({}), json!([]), json!("x"), json!(7)] {
        let answer = ask(bad.clone()).unwrap_or_else(|| panic!("{bad} got no answer"));
        assert_eq!(answer["error"]["code"], json!(-32600), "{bad} gave {answer}");
    }
    assert_eq!(ask(call(json!({"jsonrpc": "1.0"}))).unwrap()["id"], json!(1), "a usable id is echoed");
    assert_eq!(ask(call(json!({"id": true}))).unwrap()["id"], json!(null), "an unusable one isn't");
    // Valid ids and valid notifications are as before
    assert_eq!(handle_message(&json!({"jsonrpc": "2.0", "id": "s", "method": "ping"}), &never).unwrap()["id"], json!("s"));
    assert_eq!(handle_message(&json!({"jsonrpc": "2.0", "id": 0, "method": "ping"}), &never).unwrap()["id"], json!(0));
    assert_eq!(handle_message(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled"}), &never), None);
    assert_eq!(handle_message(&json!({"jsonrpc": "2.0", "id": 5, "result": {}}), &never), None, "an answer to nothing asked is let go");
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
    serve_connection(std::io::Cursor::new(input), Arc::new(Mutex::new(Lines(tx))), call, Arc::new(|_| {}));
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
    assert!(status["stats"].as_str().unwrap().contains("no stream is running"));
    assert_eq!(status["mcp"]["access"], json!("read_and_change"));
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

/// A plugin's own rule for settings that depend on each other: no highlights at all means no contrast either.
fn no_gain_no_contrast(_: &str, config: &mut Value) {
    if config["masters"]["gain"] == json!(0.0) {
        config["contrast"] = json!(1.0);
    }
}

#[test]
fn a_set_the_plugin_only_half_takes_is_undone_whole_and_leaves_nothing_to_revert() {
    let mut r = Rig::new();
    r.host.preset_rules = Some(no_gain_no_contrast);
    r.host.settings.insert(0, PluginSettings { name: "alpha".into(), title: "Alpha".into(), settings: vec![SettingSpec::switch("on", "Whether", false)] });
    r.host.plugin_configs.insert("alpha".into(), json!({"on": false}));
    let before = (r.config(), r.host.plugin_configs["alpha"].clone());
    let err = r.ask("set", json!({"changes": {"alpha.on": true, "grading.contrast": 1.4, "grading.masters.gain": 0.0}})).unwrap_err();
    assert!(err.starts_with("Nothing was changed.") && err.contains("kept contrast"), "{err}");
    assert_eq!((r.config(), r.host.plugin_configs["alpha"].clone()), before, "the plugin that was fine is back as it was too");
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([]));
}

#[test]
fn revert_puts_back_what_a_change_moved_on_its_own_and_all_together() {
    let mut r = Rig::new();
    r.host.preset_rules = Some(no_gain_no_contrast);
    r.host.plugin_configs.get_mut("grading").unwrap()["contrast"] = json!(1.4);
    let done = r.ask("set", json!({"changes": {"grading.masters.gain": 0.0}})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.0), "the rule moved contrast too");
    let ids: Vec<&str> = done["changed"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["grading.contrast", "grading.masters.gain"], "the answer says so");
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"].as_array().unwrap().len(), 2, "{back}");
    assert_eq!((r.config()["contrast"].clone(), r.config()["masters"]["gain"].clone()), (json!(1.4), json!(1.0)));
    assert_eq!(r.host.calls.iter().filter(|c| c.starts_with("apply_preset grading")).count(), 2, "one write to set, one to put back");
}

#[test]
fn each_connection_reverts_only_its_own_changes() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    assert_ne!(a.session(), b.session());
    r.ask_as(a.session(), "set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    // Another connection can't take it back
    let nothing = r.ask_as(b.session(), "revert", json!({})).unwrap();
    assert_eq!((nothing["reverted"].clone(), r.config()["contrast"].clone()), (json!([]), json!(1.4)));
    // A leaves; B starts from where A left it
    drop(a);
    r.deliver();
    r.ask_as(b.session(), "set", json!({"changes": {"grading.contrast": 1.6}})).unwrap();
    let back = r.ask_as(b.session(), "revert", json!({})).unwrap();
    assert_eq!(back["reverted"][0], json!({"id": "grading.contrast", "back_to": 1.4}));
    assert_eq!(r.config()["contrast"], json!(1.4));
    assert_eq!(r.host.notifications.len(), 1, "a connection going is not announced as one arriving");
    // What a gone connection changed stays, so the header button can still undo it
    assert!(r.plugin.journal.iter().any(|c| c.session == 1));
    r.plugin.undo_all(&mut r.host);
    assert_eq!(r.config()["contrast"], json!(1.0));
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
fn picking_another_phone_is_left_to_the_person_too() {
    let mut r = Rig::new();
    let action = |id: &str| ActionSpec { id: id.into(), label: id.into(), group: "Phone".into(), params: vec![], repeats: false, on_release: false, current: None, keywords: String::new() };
    r.host.actions = vec![("connection".into(), action("phone.pick")), ("grading".into(), action("grading.reset"))];
    let listed = r.ask("actions", json!({})).unwrap();
    let ids: Vec<&str> = listed["actions"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["grading.reset"]);
    assert!(r.ask("run_action", json!({"action": "phone.pick"})).unwrap_err().contains("left to the person"));
    assert!(!r.host.calls.iter().any(|c| c.starts_with("run_action")));
}

#[test]
fn run_action_runs_one_and_refuses_the_ones_left_to_the_person() {
    let mut r = Rig::new();
    with_actions(&mut r);
    let said = r.ask("run_action", json!({"action": "grading.reset"})).unwrap();
    assert_eq!(said["said"], json!("grading.reset ran"));
    assert!(said.get("changed").is_none() && said["note"].as_str().unwrap().contains("nothing to put back"), "an action that moved no setting says revert can't undo it: {said}");
    for id in OUT_OF_REACH {
        let why = r.ask("run_action", json!({"action": id})).unwrap_err();
        assert!(why.contains("left to the person"), "{why}");
    }
    assert!(!r.host.calls.iter().any(|c| c.starts_with("run_action") && (c.contains("start_stream") || c.contains("stop"))));
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
    assert!(bad(&mut r, json!({"ammount": 3})).contains("has no setting 'ammount'"));
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

// ── Stats and watch: the real numbers, through core's snapshot only ──

use telescope_core::stats::{Counts, Drops, Rates, Snapshot, SourceSeries, SourceSnapshot};

fn snapshot_of(fps: Vec<f32>, lost: u64) -> Snapshot {
    let n = fps.len();
    let mut drops = vec![Drops::default(); n];
    if let Some(last) = drops.last_mut() {
        last.lost_on_link = lost;
    }
    Snapshot {
        at_ms: 1000 * n as u64,
        detailed: false,
        sources: vec![SourceSnapshot {
            source_id: "phone".into(), seconds: n as u64, target_fps: 30, totals: Counts { received: 900, delivered: 880, ..Counts::default() },
            drops: drops.last().copied().unwrap_or_default(), now: Rates { received_fps: *fps.last().unwrap_or(&0.0), ..Rates::default() },
            series: SourceSeries {
                received_fps: fps.clone(), delivered_fps: fps.clone(), decoded_fps: fps, mbps: vec![9.0; n], dropped_fps: vec![0.0; n], repeated_fps: vec![0.0; n],
                decode_ms: vec![5.0; n], step_ms: vec![2.0; n], camera_ms: vec![1.0; n], drop_totals: drops, ..SourceSeries::default()
            },
            ..SourceSnapshot::default()
        }],
        ..Snapshot::default()
    }
}

impl Rig {
    fn stats_arrive(&mut self, snapshot: Snapshot) {
        self.plugin.on_event(&mut self.host, &Event::StatsUpdated { snapshot });
    }

    /// The props last handed to a piece of the plugin's UI.
    fn props(&self, ui: &str) -> Value {
        let prefix = format!("set_ui_props {ui} ");
        self.host.calls.iter().rev().find_map(|c| c.strip_prefix(&prefix).map(|j| serde_json::from_str(j).unwrap())).unwrap_or(Value::Null)
    }

    fn ui(&mut self, ui: &str, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: ui.into(), callback: callback.into(), args });
    }

    fn touched(&self) -> Vec<(Vec<String>, bool)> {
        self.host.events.iter().filter_map(|e| match e {
            Event::SettingsTouched { ids, changed } => Some((ids.clone(), *changed)),
            _ => None,
        }).collect()
    }

    fn read_only(&mut self) {
        self.plugin.set_config(&json!({"access": "read_only"})).unwrap();
    }
}

use telescope_core::ui::UiEvent;

#[test]
fn stats_gives_the_real_numbers_as_min_average_max_with_the_worst_moments() {
    let mut r = Rig::new();
    let mut fps = vec![30.0; 20];
    fps[14] = 11.0;
    r.stats_arrive(snapshot_of(fps, 6));
    let said = r.ask("stats", json!({"window_seconds": 20})).unwrap();
    let stream = &said["streams"][0];
    assert_eq!((stream["source"].clone(), stream["target_fps"].clone()), (json!("phone"), json!(30)));
    let received = &stream["series"]["received_fps"];
    assert_eq!((received["min"].as_f64(), received["max"].as_f64(), received["samples"].as_u64()), (Some(11.0), Some(30.0), Some(20)));
    assert_eq!(received["worst_moments"][0], json!({"seconds_ago": 5, "value": 11.0}), "{received}");
    assert_eq!(stream["dropped_in_window"], json!({"total": 6, "by_reason": {"lost_on_link": 6}}));
    assert!(stream["series"].get("decode_ms").is_none(), "standard leaves out the nerd series");
    let nerd = r.ask("stats", json!({"level": "nerd", "series": ["decode_ms"]})).unwrap();
    assert_eq!(nerd["streams"][0]["series"]["decode_ms"]["max"].as_f64(), Some(5.0));
    assert!(r.host.calls.iter().any(|c| c == "want_stats mcp true"), "the costly numbers are asked for, only while a nerd call wants them");
    assert!(r.host.is_scheduled("unwant"));
}

#[test]
fn stats_without_a_stream_or_with_a_bad_window_says_what_to_do() {
    let mut r = Rig::new();
    assert!(r.ask("stats", json!({})).unwrap_err().contains("No stream is running"));
    r.stats_arrive(Snapshot::default());
    assert!(r.ask("stats", json!({})).unwrap_err().contains("No stream is running"), "a snapshot with no streams is no numbers");
    r.stats_arrive(snapshot_of(vec![30.0; 10], 0));
    assert!(r.ask("stats", json!({"window_seconds": 2})).unwrap_err().contains("5 to 60"));
    assert!(r.ask("stats", json!({"level": "loud"})).unwrap_err().contains("standard or nerd"));
    assert!(r.ask("stats", json!({"series": 3})).unwrap_err().contains("list of names"));
}

#[test]
fn stats_work_with_the_stats_plugin_switched_off() {
    let mut r = Rig::new();
    assert!(!r.host.plugins.iter().any(|p| p == "stats"));
    r.stats_arrive(snapshot_of(vec![30.0; 10], 0));
    assert!(r.ask("stats", json!({})).is_ok());
}

#[test]
fn watch_collects_a_second_at_a_time_and_answers_when_the_time_is_up() {
    let mut r = Rig::new();
    let mut step = 0;
    let said = r.ask_with("watch", json!({"seconds": 10}), |r| {
        if !r.host.scheduled.iter().any(|(ms, t)| *ms == 10_000 && t.starts_with("watch:")) || step > 0 {
            return;
        }
        step = 1;
        for second in 1..=8 {
            let mut fps = vec![30.0; second];
            if second == 5 {
                fps[4] = 8.0;
            }
            r.stats_arrive(snapshot_of(fps, if second >= 6 { 3 } else { 0 }));
        }
        let token = r.host.scheduled.iter().find(|(_, t)| t.starts_with("watch:")).unwrap().1.clone();
        r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token });
    }).unwrap();
    let received = &said["streams"][0]["series"]["received_fps"];
    assert_eq!((received["samples"].as_u64(), received["min"].as_f64()), (Some(8), Some(8.0)), "{said}");
    assert_eq!(received["worst_moments"][0], json!({"seconds_ago": 3, "value": 8.0}));
    assert_eq!(said["streams"][0]["dropped_in_window"]["total"], json!(3));
}

#[test]
fn a_watch_with_no_stream_says_so_and_a_bad_length_is_refused() {
    let mut r = Rig::new();
    assert!(r.ask("watch", json!({"seconds": 1})).unwrap_err().contains("5 to 120"));
    assert!(r.ask("watch", json!({"seconds": 500})).unwrap_err().contains("5 to 120"));
    let mut fired = false;
    let said = r.ask_with("watch", json!({"seconds": 5, "level": "nerd"}), |r| {
        if let (Some((_, token)), false) = (r.host.scheduled.iter().find(|(_, t)| t.starts_with("watch:")).cloned(), fired) {
            fired = true;
            r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token });
        }
    });
    assert!(said.unwrap_err().contains("No stream was running"));
    assert!(r.host.calls.iter().any(|c| c == "want_stats mcp true") && r.host.calls.iter().any(|c| c == "want_stats mcp false"), "nerd asks while it watches and lets go after");
}

// ── Access level ─────────────────────────────────────────────────────

#[test]
fn the_access_level_is_saved_defaults_to_read_and_change_and_ignores_what_it_does_not_know() {
    let mut p = McpPlugin::new(McpHandle::default());
    assert_eq!((p.access(), p.get_config()), (Access::ReadAndChange, json!({"access": "read_and_change", "journal": []})));
    p.set_config(&json!({"access": "read_only"})).unwrap();
    assert_eq!((p.access(), p.get_config()), (Access::ReadOnly, json!({"access": "read_only", "journal": []})));
    p.set_config(&json!({"access": "everything"})).unwrap();
    assert_eq!(p.access(), Access::ReadAndChange);
    p.set_config(&json!({})).unwrap();
    assert_eq!(p.access(), Access::ReadAndChange);
}

#[test]
fn read_only_refuses_set_and_changes_nothing() {
    let mut r = Rig::new();
    r.read_only();
    let before = r.config();
    let why = r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap_err();
    assert!(why.contains("read only") && why.contains("set was refused"), "{why}");
    assert_eq!(r.config(), before);
    assert!(!r.host.calls.iter().any(|c| c.starts_with("apply_preset") || c == "schedule_save"));
    assert!(r.touched().is_empty(), "a refused call outlines nothing");
}

#[test]
fn read_only_refuses_revert_and_leaves_the_journal() {
    let mut r = Rig::new();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    r.read_only();
    let why = r.ask("revert", json!({})).unwrap_err();
    assert!(why.contains("read only") && why.contains("revert was refused"), "{why}");
    assert_eq!(r.config()["contrast"], json!(1.4));
    assert!(r.plugin.can_undo(), "the person's button can still undo it");
}

#[test]
fn read_only_refuses_run_action_and_so_auto_colour_grading() {
    let mut r = Rig::new();
    with_actions(&mut r);
    r.read_only();
    let why = r.ask("run_action", json!({"action": "grading.reset"})).unwrap_err();
    assert!(why.contains("read only") && why.contains("run_action was refused"), "{why}");
    assert!(!r.host.calls.iter().any(|c| c.starts_with("run_action")), "nothing ran");
    // Listing what exists is only reading
    assert!(r.ask("actions", json!({})).is_ok());
}

#[test]
fn read_only_still_reads_everything() {
    let mut r = Rig::new();
    r.read_only();
    r.stats_arrive(snapshot_of(vec![30.0; 10], 0));
    for (tool, args) in [("status", json!({})), ("describe", json!({})), ("get", json!({})), ("diagnostics", json!({})), ("actions", json!({})), ("stats", json!({}))] {
        assert!(r.ask(tool, args).is_ok(), "{tool} is a read");
    }
}

#[test]
fn changing_the_level_applies_to_the_next_call_and_the_page_picks_it() {
    let mut r = Rig::new();
    assert!(r.ask("set", json!({"changes": {"grading.contrast": 1.2}})).is_ok());
    r.ui(TAB, "access-picked", vec![json!(0)]);
    assert_eq!(r.plugin.access(), Access::ReadOnly);
    assert_eq!(r.props(TAB)["access-current"], json!(0));
    assert!(r.host.calls.iter().any(|c| c == "schedule_save"), "the level is saved");
    assert!(r.ask("set", json!({"changes": {"grading.contrast": 1.3}})).is_err());
    r.ui(TAB, "access-picked", vec![json!(1)]);
    assert!(r.ask("set", json!({"changes": {"grading.contrast": 1.3}})).is_ok());
}

// ── The activity log ─────────────────────────────────────────────────

#[test]
fn the_log_says_in_plain_words_what_each_call_did_newest_first() {
    let mut r = Rig::new();
    let client = r.handle.open_client().unwrap();
    r.handle.name_client(client.session(), "Claude Desktop");
    let s = client.session();
    r.ask_as(s, "status", json!({})).unwrap();
    r.ask_as(s, "get", json!({"ids": ["grading.contrast"]})).unwrap();
    r.ask_as(s, "set", json!({"changes": {"grading.contrast": 1.4, "grading.on": false}})).unwrap();
    r.ask_as(s, "set", json!({"changes": {"grading.contrast": 9}})).unwrap_err();
    let texts: Vec<String> = r.plugin.log().into_iter().map(|e| e.text).collect();
    assert_eq!(texts, ["Failed: Set Contrast to 9", "Set Contrast to 1.4, Colour grading to off", "Read Contrast", "Read status"]);
    assert!(r.plugin.log().iter().all(|e| e.who == "Claude Desktop" && !e.refused));
    assert_eq!(r.plugin.log()[0].time.len(), 8, "a time of day");
    // The page gets the same rows
    let rows = r.props(TAB)["log"].clone();
    assert_eq!(rows[0]["text"], json!("Failed: Set Contrast to 9"));
    assert_eq!(rows[3]["text"], json!("Read status"));
}

#[test]
fn a_refused_call_is_marked_in_the_log_and_says_what_it_tried() {
    let mut r = Rig::new();
    r.read_only();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap_err();
    let entry = r.plugin.log().remove(0);
    assert!(entry.refused);
    assert_eq!(entry.text, "Refused (read only): Set Contrast to 1.4");
    assert_eq!(entry.who, "An AI app", "an app that never said its name");
    assert_eq!(r.props(TAB)["log"][0]["kind"], json!("refused"));
}

#[test]
fn the_log_keeps_the_last_thirty_and_only_in_memory() {
    let mut r = Rig::new();
    for _ in 0..45 {
        r.ask("status", json!({})).unwrap();
    }
    assert_eq!(r.plugin.log().len(), LOG_KEPT);
    assert_eq!(r.props(TAB)["log"].as_array().unwrap().len(), 30);
    assert_eq!(r.plugin.get_config(), json!({"access": "read_and_change", "journal": []}), "nothing of it is in what gets saved");
}

#[test]
fn a_long_list_of_settings_is_cut_short_in_the_log() {
    let mut r = Rig::new();
    r.ask("get", json!({"ids": ["grading.on", "grading.contrast", "grading.masters.gain", "grading.mode"]})).unwrap();
    assert_eq!(r.plugin.log()[0].text, "Read Colour grading, Contrast, Masters gain and 1 more");
}

// ── The header button ────────────────────────────────────────────────

#[test]
fn the_button_knows_whether_an_ai_app_is_connected() {
    let mut r = Rig::new();
    assert_eq!(r.plugin.header_widget().unwrap().props["connected"], json!(false));
    let client = r.handle.open_client().unwrap();
    r.deliver();
    assert_eq!(r.props(HEADER)["connected"], json!(true));
    assert_eq!(r.props(TAB)["clients"], json!(["An AI app"]));
    r.handle.name_client(client.session(), "Cursor");
    r.deliver();
    assert_eq!(r.props(TAB)["clients"], json!(["Cursor"]));
    drop(client);
    r.deliver();
    assert_eq!(r.props(HEADER)["connected"], json!(false));
    assert_eq!(r.props(TAB)["clients"], json!([]));
}

#[test]
fn the_button_lights_for_a_look_or_a_change_and_settles_after_a_moment() {
    let mut r = Rig::new();
    assert_eq!(r.plugin.header_widget().unwrap().props["activity"], json!(""));
    r.ask("get", json!({})).unwrap();
    assert_eq!(r.props(HEADER)["activity"], json!("read"));
    assert!(r.host.is_scheduled("calm"), "it settles on its own");
    r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    assert_eq!(r.props(HEADER)["activity"], json!("change"), "a change shows stronger than a look");
    r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: "calm".into() });
    assert_eq!(r.props(HEADER)["activity"], json!(""));
}

#[test]
fn the_button_stays_lit_for_the_whole_of_a_slow_call() {
    let mut r = Rig::new();
    let mut seen = Vec::new();
    r.ask_with("measure", json!({}), |r| {
        if seen.is_empty() && r.host.events.iter().any(|e| matches!(e, Event::PictureMeasureRequested { .. })) {
            seen.push(r.props(HEADER)["activity"].clone());
            r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: "calm".into() });
            seen.push(r.props(HEADER)["activity"].clone());
            let token = r.host.events.iter().rev().find_map(|e| match e {
                Event::PictureMeasureRequested { token } => Some(token.clone()),
                _ => None,
            }).unwrap();
            r.plugin.on_event(&mut r.host, &Event::PictureMeasured { token, ok: true, message: String::new(), data: json!({}) });
        }
    }).unwrap();
    assert_eq!(seen, [json!("read"), json!("read")], "a stray settle while a call runs changes nothing");
}

#[test]
fn undo_from_the_button_puts_back_what_the_ai_changed_for_every_connection() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    assert_eq!(r.props(HEADER)["can-undo"], json!(false), "nothing to undo yet");
    r.ask_as(a.session(), "set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    r.ask_as(b.session(), "set", json!({"changes": {"grading.masters.gain": 0.5}})).unwrap();
    assert_eq!(r.props(HEADER)["can-undo"], json!(true));
    r.ui(HEADER, "undo", vec![]);
    assert_eq!((r.config()["contrast"].clone(), r.config()["masters"]["gain"].clone()), (json!(1.0), json!(1.0)), "both apps' changes are back");
    assert!(!r.plugin.can_undo());
    assert_eq!(r.props(HEADER)["can-undo"], json!(false));
    let entry = r.plugin.log().remove(0);
    assert_eq!((entry.who.as_str(), entry.text.as_str()), ("You", "Undid what the AI changed"));
    assert!(r.touched().last().is_some_and(|(ids, changed)| *changed && ids.contains(&"grading.contrast".to_string())), "the controls that moved back are outlined");
}

#[test]
fn undo_goes_back_through_connections_to_where_a_shared_setting_began() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    r.ask_as(a.session(), "set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    r.ask_as(b.session(), "set", json!({"changes": {"grading.contrast": 1.7}})).unwrap();
    r.ui(HEADER, "undo", vec![]);
    assert_eq!(r.config()["contrast"], json!(1.0));
}

#[test]
fn undo_leaves_what_the_person_moved_by_hand_and_works_in_read_only() {
    let mut r = Rig::new();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4, "grading.masters.gain": 0.5}})).unwrap();
    r.host.plugin_configs.get_mut("grading").unwrap()["masters"]["gain"] = json!(0.2);
    r.read_only();
    r.ui(HEADER, "undo", vec![]);
    assert_eq!((r.config()["contrast"].clone(), r.config()["masters"]["gain"].clone()), (json!(1.0), json!(0.2)));
}

#[test]
fn the_buttons_open_side_opens_the_mcp_page_and_copy_puts_the_entry_on_the_clipboard() {
    let mut r = Rig::new();
    r.ui(HEADER, "open", vec![]);
    assert!(r.host.calls.iter().any(|c| c == "open_settings mcp"));
    r.ui(TAB, "copy-clicked", vec![]);
    let entry = McpPlugin::connect_entry();
    assert!(r.host.calls.iter().any(|c| *c == format!("copy_text {entry}")), "{:?}", r.host.calls);
    let json: Value = serde_json::from_str(&format!("{{{entry}}}")).expect("a JSON entry to paste inside mcpServers");
    assert_eq!(json["telescope"]["args"], json!(["mcp"]));
    assert!(json["telescope"]["command"].as_str().is_some_and(|c| !c.is_empty()));
}

// ── Outlines: which controls the AI touched ──────────────────────────

#[test]
fn a_set_outlines_every_setting_it_changed_including_the_ones_that_moved_with_it() {
    let mut r = Rig::new();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4, "grading.masters.gain": 0.8}})).unwrap();
    let (ids, changed) = r.touched().remove(0);
    assert!(changed);
    assert_eq!(ids, ["grading.contrast", "grading.masters.gain"]);
}

#[test]
fn a_get_of_a_few_named_settings_is_outlined_as_a_look_and_nothing_else_is() {
    let mut r = Rig::new();
    r.ask("get", json!({"ids": ["grading.contrast", "grading.masters.gain"]})).unwrap();
    assert_eq!(r.touched(), [(vec!["grading.contrast".to_string(), "grading.masters.gain".to_string()], false)]);
    // Looking at everything, listing, status, a long list and a failure make no outlines
    r.host.events.clear();
    r.ask("get", json!({})).unwrap();
    r.ask("describe", json!({})).unwrap();
    r.ask("status", json!({})).unwrap();
    r.ask("get", json!({"ids": ["grading.on", "grading.on", "grading.on", "grading.on", "grading.on", "grading.on", "grading.on"]})).unwrap();
    r.ask("get", json!({"ids": ["grading.nope"]})).unwrap_err();
    r.ask("set", json!({"changes": {"grading.contrast": 9}})).unwrap_err();
    assert!(r.touched().is_empty(), "{:?}", r.touched());
}

#[test]
fn a_revert_outlines_what_went_back() {
    let mut r = Rig::new();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    r.host.events.clear();
    r.ask("revert", json!({})).unwrap();
    assert_eq!(r.touched(), [(vec!["grading.contrast".to_string()], true)]);
}

// ── Who is connected ────────────────────────────────────────────────

#[test]
fn an_ai_app_that_says_hello_is_named_and_a_blank_name_is_not() {
    let names = Arc::new(Mutex::new(Vec::new()));
    let hello: Arc<Hello> = {
        let names = names.clone();
        Arc::new(move |n| names.lock().unwrap().push(n.to_string()))
    };
    let call: Arc<Caller> = Arc::new(|_, _| Ok(json!({})));
    let (tx, _rx) = channel();
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"clientInfo\":{\"name\":\"Claude Desktop\",\"version\":\"1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"  \"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"initialize\",\"params\":{}}\n",
    );
    serve_connection(std::io::Cursor::new(input), Arc::new(Mutex::new(Lines(tx))), call, hello);
    assert_eq!(*names.lock().unwrap(), ["Claude Desktop"]);
}

// ── The undo journal ─────────────────────────────────────────────────

fn set_contrast(r: &mut Rig, session: u64, value: f64) {
    r.ask_as(session, "set", json!({"changes": {"grading.contrast": value}})).unwrap();
}

fn by_hand(r: &mut Rig, contrast: f64) {
    let mut config = r.config();
    config["contrast"] = json!(contrast);
    r.host.edit_by_hand("grading", config);
}

#[test]
fn writing_again_after_someone_elses_edit_goes_back_to_that_edit_and_not_to_the_start() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    by_hand(&mut r, 1.7);
    set_contrast(&mut r, 0, 1.9);
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.7), "the person's 1.7 is not erased: {back}");
    assert_eq!(back["reverted"][0], json!({"id": "grading.contrast", "back_to": 1.7}));
    assert!(!r.plugin.can_undo());
}

#[test]
fn an_equal_value_write_after_someone_elses_edit_is_still_told_apart() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    by_hand(&mut r, 1.7);
    // The app moved the setting by writing 1.4 again, so that write is its own
    set_contrast(&mut r, 0, 1.4);
    r.ask("revert", json!({})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.7));
}

/// A plugin's own rule: at unity gain the contrast is put back to 1 (as zooming out puts the pan back to the middle).
fn unity_gain_resets_contrast(_: &str, config: &mut Value) {
    if config["masters"]["gain"] == json!(1.0) {
        config["contrast"] = json!(1.0);
    }
}

#[test]
fn a_revert_that_would_move_a_setting_through_a_coupled_one_leaves_the_persons_edit_alone() {
    let mut r = Rig::new();
    r.host.preset_rules = Some(unity_gain_resets_contrast);
    r.ask("set", json!({"changes": {"grading.masters.gain": 2.0, "grading.contrast": 1.5}})).unwrap();
    // The person moves the contrast; putting the gain back would reset it to 1 on its own
    by_hand(&mut r, 1.8);
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.8), "{back}");
    assert_eq!(r.config()["masters"]["gain"], json!(2.0), "the gain stays too: it could not go back without moving the contrast");
    assert_eq!(back["reverted"], json!([]));
    let left: Vec<&str> = back["left"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert!(left.contains(&"grading.contrast") && left.contains(&"grading.masters.gain"), "{back}");
    assert!(back["left"].as_array().unwrap().iter().any(|c| c["why"].as_str().unwrap().contains("would also have moved grading.contrast")), "{back}");
}

#[test]
fn a_coupled_revert_goes_ahead_when_everything_it_moves_is_going_back() {
    let mut r = Rig::new();
    r.host.preset_rules = Some(unity_gain_resets_contrast);
    r.ask("set", json!({"changes": {"grading.masters.gain": 2.0, "grading.contrast": 1.5}})).unwrap();
    r.ask("revert", json!({})).unwrap();
    assert_eq!((r.config()["contrast"].clone(), r.config()["masters"]["gain"].clone()), (json!(1.0), json!(1.0)));
}

#[test]
fn undo_all_unwinds_in_the_order_things_were_written_and_not_the_order_apps_connected() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    // B, the later connection, writes first
    set_contrast(&mut r, b.session(), 1.7);
    set_contrast(&mut r, a.session(), 1.4);
    r.ui(HEADER, "undo", vec![]);
    assert_eq!(r.config()["contrast"], json!(1.0), "back to where it began, nothing left to undo");
    assert!(!r.plugin.can_undo());
}

#[test]
fn one_apps_revert_leaves_a_setting_another_app_moved_after_it() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    set_contrast(&mut r, b.session(), 1.7);
    set_contrast(&mut r, a.session(), 1.4);
    let back = r.ask_as(b.session(), "revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([]));
    assert_eq!(r.config()["contrast"], json!(1.4), "A's later write is not B's to take back");
}

#[test]
fn a_write_that_moves_nothing_is_not_a_step_to_undo() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.0);
    assert!(!r.plugin.can_undo() && r.plugin.journal.is_empty());
    assert_eq!(r.props(HEADER)["can-undo"], json!(false));
    // Reconnecting and asking for what is already there over and over keeps nothing
    for _ in 0..5 {
        let client = r.handle.open_client().unwrap();
        r.deliver();
        set_contrast(&mut r, client.session(), 1.0);
    }
    assert!(r.plugin.journal.is_empty());
}

#[test]
fn the_same_app_moving_a_setting_again_is_one_step_and_back_where_it_began_is_none() {
    let mut r = Rig::new();
    for value in [1.1, 1.2, 1.3, 1.4] {
        set_contrast(&mut r, 0, value);
    }
    assert_eq!(r.plugin.journal.len(), 1, "a slider dragged in steps is a single step");
    assert_eq!((r.plugin.journal[0].was.clone(), r.plugin.journal[0].now.clone()), (json!(1.0), json!(1.4)));
    set_contrast(&mut r, 0, 1.0);
    assert!(r.plugin.journal.is_empty(), "it ended where it began");
    assert!(!r.plugin.can_undo());
}

#[test]
fn the_journal_keeps_the_latest_steps_and_forgets_the_oldest() {
    let mut r = Rig::new();
    let writes = JOURNAL_KEPT + 30;
    // Two apps taking turns never extend each other's step, so every write is a step of its own
    for i in 0..writes {
        let value = |n: usize| json!(0.5 + (n % 100) as f64 * 0.01);
        let was = if i == 0 { json!(1.0) } else { value(i - 1) };
        r.plugin.remember(1 + (i as u64 % 2), vec![("grading".into(), "contrast".into(), was, value(i))], None);
    }
    assert_eq!(r.plugin.journal.len(), JOURNAL_KEPT);
    assert!(r.plugin.journal.windows(2).all(|w| w[0].seq < w[1].seq), "still in the order they happened");
    assert_eq!(r.plugin.journal.last().unwrap().seq, writes as u64);
}

// ── Actions that change settings ─────────────────────────────────────

#[test]
fn a_run_action_that_moves_settings_is_journalled_outlined_and_can_be_undone() {
    let mut r = Rig::new();
    with_actions(&mut r);
    by_hand(&mut r, 1.4);
    let mut after = r.config();
    after["on"] = json!(false);
    after["contrast"] = json!(1.0);
    r.host.action_effects.insert("grading.reset".into(), ("grading".into(), after));
    let said = r.ask("run_action", json!({"action": "grading.reset"})).unwrap();
    let mut ids: Vec<&str> = said["changed"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["grading.contrast", "grading.on"], "{said}");
    assert!(r.plugin.can_undo(), "the header's undo is lit by an action's changes");
    assert_eq!(r.props(HEADER)["can-undo"], json!(true));
    let (outlined, changed) = r.touched().remove(0);
    assert!(changed && outlined.contains(&"grading.on".to_string()) && outlined.contains(&"grading.contrast".to_string()), "{outlined:?}");
    r.ask("revert", json!({})).unwrap();
    assert_eq!((r.config()["on"].clone(), r.config()["contrast"].clone()), (json!(true), json!(1.4)));
}

// ── Limits ───────────────────────────────────────────────────────────

#[test]
fn a_watch_has_a_deadline_longer_than_its_own_length_and_other_calls_stay_quick() {
    assert!(deadline("watch", &json!({})) > Duration::from_secs(30), "the default watch is 30 s");
    assert!(deadline("watch", &json!({"seconds": 120})) > Duration::from_secs(120));
    assert!(deadline("watch", &json!({"seconds": "soon"})) > Duration::from_secs(30), "a bad length is answered at once, whatever the deadline");
    assert_eq!(deadline("stats", &json!({})), CALL_WAIT);
}

#[test]
fn a_connection_runs_only_so_many_calls_at_once_and_refuses_the_rest_with_a_reason() {
    let (release, wait) = channel::<()>();
    let wait = Mutex::new(wait);
    let call: Arc<Caller> = Arc::new(move |_, _| {
        let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(5));
        Ok(json!({"done": true}))
    });
    let (tx, rx) = channel();
    let input: String = (1..=PER_CONNECTION + 1)
        .map(|id| format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"watch\"}}}}\n"))
        .collect();
    serve_connection(std::io::Cursor::new(input), Arc::new(Mutex::new(Lines(tx))), call, Arc::new(|_| {}));
    let refused: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
    assert_eq!(refused["id"], json!(PER_CONNECTION + 1), "the one past the limit is answered at once, while the others still run");
    assert_eq!(refused["result"]["isError"], json!(true));
    assert!(refused["result"]["content"][0]["text"].as_str().unwrap().contains("Too many calls"));
    for _ in 0..PER_CONNECTION {
        release.send(()).unwrap();
    }
    for _ in 0..PER_CONNECTION {
        let done: Value = serde_json::from_str(&rx.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        assert_eq!(done["result"]["isError"], json!(false));
    }
}

#[test]
fn calls_in_all_are_capped_and_a_call_past_the_cap_is_refused_at_once() {
    let r = Rig::new();
    let waiting: Vec<_> = (0..MAX_CALLS).map(|_| {
        let handle = r.handle.clone();
        std::thread::spawn(move || handle.call("watch", json!({})))
    }).collect();
    while r.handle.lock().waiting.len() < MAX_CALLS {
        std::thread::sleep(Duration::from_millis(1));
    }
    let why = r.handle.call("status", json!({})).unwrap_err();
    assert!(why.contains("busy"), "{why}");
    r.handle.detach();
    for w in waiting {
        assert_eq!(w.join().unwrap().unwrap_err(), off());
    }
}

#[test]
fn only_so_many_apps_connect_at_once() {
    let r = Rig::new();
    let clients: Vec<_> = (0..MAX_CLIENTS).map(|_| r.handle.open_client().unwrap()).collect();
    assert!(r.handle.open_client().err().unwrap().contains("Too many AI apps"));
    drop(clients);
    assert!(r.handle.open_client().is_ok());
}

#[test]
fn a_call_whose_connection_goes_stops_waiting_and_the_watch_behind_it_is_cancelled() {
    let mut r = Rig::new();
    let client = r.handle.open_client().unwrap();
    r.deliver();
    let (handle, session) = (r.handle.clone(), client.session());
    let waiting = std::thread::spawn(move || handle.call_as(session, "watch", json!({"seconds": 30})));
    while r.plugin.watches.is_empty() {
        r.deliver();
        std::thread::sleep(Duration::from_millis(1));
    }
    let token = r.host.scheduled.iter().find(|(_, t)| t.starts_with("watch:")).unwrap().1.clone();
    drop(client);
    assert_eq!(waiting.join().unwrap().unwrap_err(), "The AI app disconnected.");
    r.deliver();
    assert!(r.plugin.watches.is_empty() && r.plugin.open.is_empty(), "the watch is let go");
    assert!(!r.host.is_scheduled(&token), "and its timer");
    assert_eq!(r.plugin.pending, 0);
}

#[cfg(unix)]
#[test]
fn a_connection_that_stops_taking_answers_is_closed_after_the_write_wait() {
    use crate::single_instance::Conn;
    let (ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    let writer = Conn::Unix(ours);
    writer.set_write_wait(Some(Duration::from_millis(100)));
    let mut closing = Closing(writer);
    let line = vec![b'x'; 1 << 16];
    // Nobody reads the other end, so the buffers fill and a write gives up instead of waiting for ever
    let failed = (0..4096).any(|_| closing.write_all(&line).is_err());
    assert!(failed, "the write timed out");
    theirs.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut sink = vec![0u8; 1 << 16];
    let ended = loop {
        match theirs.read(&mut sink) {
            Ok(0) => break true,
            Ok(_) => {}
            Err(_) => break false,
        }
    };
    assert!(ended, "the other end sees the connection close once it has read what was sent");
}

// ── The per-plugin times ─────────────────────────────────────────────

fn asked_for_times(r: &Rig) -> Vec<bool> {
    r.host.calls.iter().filter_map(|c| c.strip_prefix("want_stats mcp ").map(|on| on == "true")).collect()
}

#[test]
fn a_standard_watch_does_not_keep_the_nerd_times_on_after_the_nerd_ask_ran_out() {
    let mut r = Rig::new();
    r.stats_arrive(snapshot_of(vec![30.0; 10], 0));
    r.ask("stats", json!({"level": "nerd"})).unwrap();
    assert_eq!(asked_for_times(&r), [true]);
    // A standard watch starts while the nerd ask is fresh, and the ask runs out meanwhile
    let mut step = 0;
    r.ask_with("watch", json!({"seconds": 24}), |r| {
        if step == 0 && r.plugin.watches.len() == 1 {
            step = 1;
            r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: T_UNWANT.into() });
            assert_eq!(asked_for_times(r), [true, false], "the standard watch has no use for the times");
            let token = r.host.scheduled.iter().find(|(_, t)| t.starts_with("watch:")).unwrap().1.clone();
            r.stats_arrive(snapshot_of(vec![30.0; 3], 0));
            r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token });
        }
    }).unwrap();
    assert_eq!(asked_for_times(&r), [true, false], "and finishing it leaves nothing asked for");
}

#[test]
fn a_nerd_watch_keeps_the_times_on_when_the_nerd_ask_runs_out_and_lets_go_when_it_ends() {
    let mut r = Rig::new();
    r.stats_arrive(snapshot_of(vec![30.0; 10], 0));
    r.ask("stats", json!({"level": "nerd"})).unwrap();
    let mut step = 0;
    r.ask_with("watch", json!({"seconds": 20, "level": "nerd"}), |r| {
        if step == 0 && r.plugin.watches.len() == 1 {
            step = 1;
            r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: T_UNWANT.into() });
            assert_eq!(asked_for_times(r), [true], "the nerd watch still needs them");
            let token = r.host.scheduled.iter().find(|(_, t)| t.starts_with("watch:")).unwrap().1.clone();
            r.stats_arrive(snapshot_of(vec![30.0; 3], 0));
            r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token });
        }
    }).unwrap();
    assert_eq!(asked_for_times(&r), [true, false]);
}

// ── Undo covers what actions change, survives a restart and tells the person's edits from its own ──

fn blur_action(r: &mut Rig) {
    let action = ActionSpec { id: "privacy_blur.auto".into(), label: "Privacy blur when someone walks in".into(), group: "Privacy blur".into(), params: vec![], repeats: false,
                              on_release: false, current: None, keywords: String::new() };
    r.host.actions = vec![("blur".into(), action)];
    // No declared settings, like the real plugin: only its saved config holds the switch
    r.host.plugin_configs.insert("blur".into(), json!({"on": true, "mode": "whole"}));
    r.host.plugins.push("blur".into());
    r.host.action_effects.insert("privacy_blur.auto".into(), ("blur".into(), json!({"on": false, "mode": "whole"})));
}

#[test]
fn an_action_that_turns_off_something_no_setting_declares_is_put_back_by_revert() {
    let mut r = Rig::new();
    blur_action(&mut r);
    let said = r.ask("run_action", json!({"action": "privacy_blur.auto"})).unwrap();
    assert_eq!(said["changed"][0], json!({"id": "blur.on", "was": true, "now": false}), "{said}");
    assert!(r.plugin.can_undo());
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([{"id": "blur.on", "back_to": true}]), "{back}");
    assert_eq!(r.host.plugin_configs["blur"], json!({"on": true, "mode": "whole"}), "automatic privacy blur is on again");
    assert!(!r.plugin.can_undo());
}

#[test]
fn the_headers_undo_puts_back_an_actions_undeclared_state_beside_a_declared_setting() {
    let mut r = Rig::new();
    blur_action(&mut r);
    r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    r.ask("run_action", json!({"action": "privacy_blur.auto"})).unwrap();
    r.ui(HEADER, "undo", vec![]);
    assert_eq!((r.config()["contrast"].clone(), r.host.plugin_configs["blur"]["on"].clone()), (json!(1.0), json!(true)));
}

#[test]
fn an_action_that_moves_an_undeclared_key_of_a_plugin_that_declares_others_puts_it_back_too() {
    let mut r = Rig::new();
    with_actions(&mut r);
    let mut grading = r.config();
    grading["lut"] = json!("other.cube");
    r.host.action_effects.insert("grading.reset".into(), ("grading".into(), grading));
    let said = r.ask("run_action", json!({"action": "grading.reset"})).unwrap();
    assert_eq!(said["changed"], json!([{"id": "grading.lut", "was": "keep.cube", "now": "other.cube"}]));
    r.ask("revert", json!({})).unwrap();
    assert_eq!(r.config()["lut"], json!("keep.cube"));
}

#[test]
fn an_action_that_changes_nothing_saved_is_named_when_undo_cannot_put_it_back() {
    let mut r = Rig::new();
    with_actions(&mut r);
    r.ask("run_action", json!({"action": "grading.reset"})).unwrap();
    assert!(!r.plugin.can_undo(), "there is nothing to undo, so the button isn't lit for it");
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([]));
    assert_eq!(back["not_restored"][0]["action"], json!("grading.reset"), "{back}");
    assert!(back["not_restored"][0]["why"].as_str().unwrap().contains("nothing to put back"));
    assert!(r.ask("revert", json!({})).unwrap().get("not_restored").is_none(), "it is told once");
    // With a setting moved too, the button's undo says what it left
    with_actions(&mut r);
    r.ask("run_action", json!({"action": "grading.reset"})).unwrap();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    r.ui(HEADER, "undo", vec![]);
    assert_eq!(r.plugin.log().remove(0).text, "Undid what the AI changed, except 1 it couldn't put back");
}

#[test]
fn the_journal_is_saved_with_the_config_and_undo_still_works_after_a_restart() {
    let mut r = Rig::new();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4, "grading.masters.gain": 0.5}})).unwrap();
    let saved: Value = serde_json::from_str(&r.plugin.get_config().to_string()).unwrap();
    assert_eq!((saved["access"].clone(), saved["journal"].as_array().unwrap().len()), (json!("read_and_change"), 2));
    // Telescope starts again with the settings as the AI left them
    let mut after = Rig::new();
    after.host.edit_by_hand("grading", r.config());
    after.plugin.set_config(&saved).unwrap();
    assert!(after.plugin.can_undo(), "the button is lit after the restart");
    // An AI app that connects now is nobody who made those changes
    assert_eq!(after.ask_as(1, "revert", json!({})).unwrap()["reverted"], json!([]));
    after.ask_as(1, "set", json!({"changes": {"grading.contrast": 1.2}})).unwrap();
    let seqs: Vec<u64> = after.plugin.journal.iter().map(|c| c.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "new steps come after the saved ones: {seqs:?}");
    after.plugin.undo_all(&mut after.host);
    assert_eq!((after.config()["contrast"].clone(), after.config()["masters"]["gain"].clone()), (json!(1.0), json!(1.0)));
    assert!(after.host.calls.iter().any(|c| c == "schedule_save"), "the emptied journal is saved too");
}

#[test]
fn an_edit_after_a_restart_that_lands_on_the_same_value_is_not_undone() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    let saved: Value = serde_json::from_str(&r.plugin.get_config().to_string()).unwrap();
    let mut after = Rig::new();
    after.host.edit_by_hand("grading", r.config());
    after.plugin.set_config(&saved).unwrap();
    after.plugin.on_plugins_ready(&mut after.host);
    // The person moves it and comes back to the AI's value
    by_hand(&mut after, 1.7);
    by_hand(&mut after, 1.4);
    after.plugin.undo_all(&mut after.host);
    assert_eq!(after.config()["contrast"], json!(1.4), "the person's edit stays");
    // Untouched since the restart, the same step does go back
    let mut again = Rig::new();
    again.host.edit_by_hand("grading", r.config());
    again.plugin.set_config(&saved).unwrap();
    again.plugin.on_plugins_ready(&mut again.host);
    again.plugin.undo_all(&mut again.host);
    assert_eq!(again.config()["contrast"], json!(1.0));
}

#[test]
fn a_revert_by_another_app_does_not_make_an_older_step_valid_after_the_person_edited_past_it() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    set_contrast(&mut r, a.session(), 1.4);
    by_hand(&mut r, 1.7);
    by_hand(&mut r, 1.4);
    set_contrast(&mut r, b.session(), 1.8);
    r.ask_as(b.session(), "revert", json!({})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.4));
    let back = r.ask_as(a.session(), "revert", json!({})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.4), "the person's 1.4 is not overwritten: {back}");
    assert_eq!(back["reverted"], json!([]));
}

/// Saves the journal, starts a new run on the settings as they stand and hears the plugins are ready, as a restart does.
fn restarted(r: &Rig) -> Rig {
    let saved: Value = serde_json::from_str(&r.plugin.get_config().to_string()).unwrap();
    let mut after = Rig::new();
    after.host.edit_by_hand("grading", r.config());
    after.plugin.set_config(&saved).unwrap();
    after.plugin.on_plugins_ready(&mut after.host);
    after
}

#[test]
fn an_edit_between_two_steps_still_blocks_the_older_one_after_a_restart() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    set_contrast(&mut r, a.session(), 1.4);
    by_hand(&mut r, 1.7);
    by_hand(&mut r, 1.4);
    set_contrast(&mut r, b.session(), 1.8);
    let mut after = restarted(&r);
    after.plugin.undo_all(&mut after.host);
    assert_eq!(after.config()["contrast"], json!(1.4), "the newest step goes back and the person's 1.4 is not overwritten");
}

#[test]
fn two_steps_nobody_edited_between_both_go_back_after_a_restart() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    set_contrast(&mut r, a.session(), 1.4);
    set_contrast(&mut r, b.session(), 1.8);
    let mut after = restarted(&r);
    after.plugin.undo_all(&mut after.host);
    assert_eq!(after.config()["contrast"], json!(1.0));
}

#[test]
fn what_cuts_a_chain_is_kept_through_a_second_restart() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    set_contrast(&mut r, a.session(), 1.4);
    by_hand(&mut r, 1.7);
    by_hand(&mut r, 1.4);
    set_contrast(&mut r, b.session(), 1.8);
    let once = restarted(&r);
    let mut twice = restarted(&once);
    twice.plugin.undo_all(&mut twice.host);
    assert_eq!(twice.config()["contrast"], json!(1.4));
}

#[test]
fn a_journal_from_before_chains_were_saved_only_undoes_the_newest_step_on_a_setting() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"journal": [
        {"seq": 1, "plugin": "grading", "key": "contrast", "source": "phone", "was": 1.0, "now": 1.4},
        {"seq": 2, "plugin": "grading", "key": "contrast", "source": "phone", "was": 1.4, "now": 1.8},
    ]})).unwrap();
    by_hand(&mut r, 1.8);
    r.plugin.on_plugins_ready(&mut r.host);
    r.plugin.undo_all(&mut r.host);
    assert_eq!(r.config()["contrast"], json!(1.4));
}

#[test]
fn a_revert_by_another_app_still_leaves_an_untouched_older_step_valid() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    set_contrast(&mut r, a.session(), 1.4);
    set_contrast(&mut r, b.session(), 1.8);
    r.ask_as(b.session(), "revert", json!({})).unwrap();
    let back = r.ask_as(a.session(), "revert", json!({})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.0), "{back}");
}

#[test]
fn what_changes_the_journal_asks_for_a_save() {
    let mut r = Rig::new();
    let saves = |r: &Rig| r.host.calls.iter().filter(|c| *c == "schedule_save").count();
    r.ask("set", json!({"changes": {"grading.contrast": 1.4}})).unwrap();
    let after_set = saves(&r);
    with_actions(&mut r);
    r.ask("run_action", json!({"action": "grading.reset"})).unwrap();
    assert!(saves(&r) > after_set, "an action's steps are saved");
    let before_revert = saves(&r);
    r.ask("revert", json!({})).unwrap();
    assert!(saves(&r) > before_revert);
}

#[test]
fn a_saved_journal_with_a_broken_entry_still_loads_the_rest() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"journal": [{"seq": 3, "plugin": "grading", "key": "contrast", "source": "phone", "was": 1.0, "now": 1.4}, {"nonsense": true}, 7]})).unwrap();
    assert_eq!(r.plugin.journal.len(), 1);
    assert_eq!(r.plugin.journal[0].session, PREVIOUS_RUN);
    r.plugin.set_config(&json!({"access": "read_only"})).unwrap();
    assert_eq!(r.plugin.journal.len(), 1, "a config with no journal in it leaves the one there is");
}

#[test]
fn an_edit_that_lands_on_the_ais_value_again_is_the_persons_and_is_left_alone() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    by_hand(&mut r, 1.7);
    by_hand(&mut r, 1.4);
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([]), "{back}");
    assert!(back["left"][0]["why"].as_str().unwrap().contains("edited since"), "{back}");
    assert_eq!(r.config()["contrast"], json!(1.4), "the person's value stays");
}

#[test]
fn the_persons_edit_of_another_setting_does_not_stop_the_revert_of_this_one() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    let mut config = r.config();
    config["masters"]["lift"] = json!(0.2);
    r.host.edit_by_hand("grading", config);
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([{"id": "grading.contrast", "back_to": 1.0}]), "{back}");
    assert_eq!(r.config()["masters"]["lift"], json!(0.2));
}

#[test]
fn writing_again_after_the_person_landed_on_the_same_value_is_not_folded_into_the_first_step() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    by_hand(&mut r, 1.7);
    by_hand(&mut r, 1.4);
    set_contrast(&mut r, 0, 1.2);
    assert_eq!(r.plugin.journal.len(), 2, "the person's edits are between the two writes");
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.4), "back to where the person left it, not to the start: {back}");
    assert_eq!(back["reverted"], json!([{"id": "grading.contrast", "back_to": 1.4}]));
    assert!(back["left"][0]["why"].as_str().unwrap().contains("edited since"));
}

#[test]
fn two_apps_writing_one_after_the_other_are_still_undone_together_with_revisions_counted() {
    let mut r = Rig::new();
    let (a, b) = (r.handle.open_client().unwrap(), r.handle.open_client().unwrap());
    r.deliver();
    set_contrast(&mut r, a.session(), 1.4);
    set_contrast(&mut r, b.session(), 1.7);
    set_contrast(&mut r, a.session(), 1.9);
    r.plugin.undo_all(&mut r.host);
    assert_eq!(r.config()["contrast"], json!(1.0));
}

#[test]
fn a_change_for_a_camera_that_is_not_shown_stays_in_the_journal_and_goes_back_when_it_is() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    r.host.focused = Some("other phone".into());
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([]));
    assert!(back["left"][0]["why"].as_str().unwrap().contains("kept"), "it doesn't say it is gone: {back}");
    assert!(r.plugin.can_undo(), "the change is still there to undo");
    assert_eq!(r.props(HEADER)["can-undo"], json!(true));
    // The button's undo keeps it too
    r.ui(HEADER, "undo", vec![]);
    assert!(r.plugin.can_undo());
    assert_eq!(r.config()["contrast"], json!(1.4));
    r.host.focused = Some("phone".into());
    let back = r.ask("revert", json!({})).unwrap();
    assert_eq!(back["reverted"], json!([{"id": "grading.contrast", "back_to": 1.0}]), "{back}");
    assert!(!r.plugin.can_undo());
}

#[test]
fn a_kept_change_stays_in_order_beside_the_ones_that_were_undone() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    r.host.focused = Some("other phone".into());
    r.ask("set", json!({"changes": {"grading.masters.gain": 0.5}})).unwrap();
    r.host.focused = Some("phone".into());
    r.ask("revert", json!({})).unwrap();
    assert_eq!((r.config()["contrast"].clone(), r.config()["masters"]["gain"].clone()), (json!(1.0), json!(0.5)));
    let kept: Vec<&str> = r.plugin.journal.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(kept, ["masters.gain"]);
}

#[test]
fn a_change_for_a_plugin_that_is_switched_off_is_kept_until_it_is_back() {
    let mut r = Rig::new();
    set_contrast(&mut r, 0, 1.4);
    let config = r.host.plugin_configs.remove("grading").unwrap();
    let back = r.ask("revert", json!({})).unwrap();
    assert!(back["left"][0]["why"].as_str().unwrap().contains("switched off"), "{back}");
    assert!(r.plugin.can_undo());
    r.host.plugin_configs.insert("grading".into(), config);
    r.ask("revert", json!({})).unwrap();
    assert_eq!(r.config()["contrast"], json!(1.0));
}
