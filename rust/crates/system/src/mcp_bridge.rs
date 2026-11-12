//! `telescope mcp`: what an AI app starts. It speaks MCP over stdin and stdout, one JSON-RPC message per line, and passes every message on
//! to the running Telescope over a connection that stays open on the single-instance socket (mcp.rs answers there). No network port is
//! opened. When Telescope isn't running, or its MCP server is off, the bridge still answers the AI app, with the reason in words, and
//! connects again at the next request, so starting Telescope or switching the server on needs no restart of the AI app.

use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The two halves of a connection to Telescope.
pub struct Pipe {
    pub read: Box<dyn Read + Send>,
    pub write: Box<dyn Write + Send>,
}

type Output = Arc<Mutex<Box<dyn Write + Send>>>;

/// The longest message taken from one line, the same limit Telescope puts on a request.
const MAX_LINE: usize = crate::mcp::MAX_LINE;

/// One line read with a cap.
enum Line {
    Text(String),
    /// Longer than the cap: the line was thrown away as it arrived.
    TooLong,
    End,
}

/// Reads up to the next newline but keeps no more than `max` bytes: the rest of a longer line is skipped as it comes in, so a sender that
/// never ends a line can't make this grow.
fn read_line(reader: &mut impl BufRead, max: usize) -> std::io::Result<Line> {
    let mut line: Vec<u8> = Vec::new();
    let (mut too_long, mut any) = (false, false);
    loop {
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            break;
        }
        any = true;
        let (upto, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(at) => (at, true),
            None => (chunk.len(), false),
        };
        if !too_long {
            if line.len() + upto > max {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(&chunk[..upto]);
            }
        }
        reader.consume(upto + usize::from(done));
        if done {
            break;
        }
    }
    if !any {
        return Ok(Line::End);
    }
    if too_long {
        return Ok(Line::TooLong);
    }
    let text = String::from_utf8_lossy(&line).into_owned();
    Ok(Line::Text(text.strip_suffix('\r').map(String::from).unwrap_or(text)))
}

/// A live connection to Telescope: where messages go, whether it's still there, and the requests it hasn't answered yet.
struct Session {
    write: Box<dyn Write + Send>,
    alive: Arc<AtomicBool>,
    pending: Arc<Mutex<HashSet<String>>>,
}

fn emit(output: &Output, message: &Value) {
    let mut out = output.lock().unwrap_or_else(|e| e.into_inner());
    let _ = out.write_all(format!("{message}\n").as_bytes()).and_then(|()| out.flush());
}

fn error(id: &Value, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}})
}

/// What the bridge says by itself while there's no connection: enough for the AI app to start and show why nothing works yet.
fn offline_answer(message: &Value, why: &str) -> Option<Value> {
    let id = message.get("id").filter(|id| !id.is_null())?;
    Some(match message["method"].as_str().unwrap_or("") {
        "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "telescope", "version": crate::version::display_version()},
            "instructions": why,
        }}),
        "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        // The catalogue stays whole while Telescope is away, so an AI app that listed the tools once doesn't go without them after it starts
        "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": crate::mcp::tools()}}),
        "tools/call" => json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": why}], "isError": true}}),
        _ => error(id, why),
    })
}

fn connect_session(connect: &mut dyn FnMut() -> Result<Pipe, String>, output: &Output) -> Result<Session, String> {
    let Pipe { read, write } = connect()?;
    let alive = Arc::new(AtomicBool::new(true));
    let pending: Arc<Mutex<HashSet<String>>> = Arc::default();
    let (output, flag, waiting) = (output.clone(), alive.clone(), pending.clone());
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(read);
        loop {
            let line = match read_line(&mut reader, MAX_LINE) {
                Ok(Line::Text(line)) => line,
                Ok(Line::TooLong) => continue,
                Ok(Line::End) | Err(_) => break,
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
            if message.get("method").is_none() {
                if let Some(id) = message.get("id") {
                    waiting.lock().unwrap_or_else(|e| e.into_inner()).remove(&id.to_string());
                }
            }
            emit(&output, &message);
        }
        flag.store(false, Ordering::SeqCst);
        // Whatever Telescope hadn't answered when it went never will be
        let lost: Vec<String> = waiting.lock().unwrap_or_else(|e| e.into_inner()).drain().collect();
        for id in lost {
            let id = serde_json::from_str::<Value>(&id).unwrap_or(Value::Null);
            emit(&output, &error(&id, "Telescope closed the connection, because it quit or its MCP server was switched off."));
        }
    });
    Ok(Session { write, alive, pending })
}

/// Reads messages from `input` until it ends, handing each to Telescope through `connect` (called again whenever the connection is gone).
/// Returns the exit code.
pub fn run(mut input: impl BufRead, output: Box<dyn Write + Send>, mut connect: impl FnMut() -> Result<Pipe, String>) -> i32 {
    let output: Output = Arc::new(Mutex::new(output));
    let mut session: Option<Session> = None;
    loop {
        let line = match read_line(&mut input, MAX_LINE) {
            Ok(Line::Text(line)) => line,
            Ok(Line::TooLong) => {
                emit(&output, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "That message is longer than 1 MiB, so it was skipped."}}));
                continue;
            }
            Ok(Line::End) | Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            emit(&output, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "That isn't JSON."}}));
            continue;
        };
        if session.as_ref().is_some_and(|s| !s.alive.load(Ordering::SeqCst)) {
            session = None;
        }
        let why = if session.is_none() {
            match connect_session(&mut connect, &output) {
                Ok(s) => {
                    session = Some(s);
                    None
                }
                Err(why) => Some(why),
            }
        } else {
            None
        };
        if let Some(why) = why {
            if let Some(answer) = offline_answer(&message, &why) {
                emit(&output, &answer);
            }
            continue;
        }
        let Some(open) = session.as_mut() else { continue };
        let request_id = message.get("id").filter(|id| !id.is_null() && message.get("method").is_some()).map(Value::to_string);
        if let Some(id) = &request_id {
            open.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone());
        }
        let sent = open.write.write_all(format!("{message}\n").as_bytes()).and_then(|()| open.write.flush());
        if sent.is_err() {
            if let Some(id) = request_id {
                open.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                emit(&output, &error(&message["id"], "Telescope closed the connection, because it quit or its MCP server was switched off."));
            }
            open.alive.store(false, Ordering::SeqCst);
        }
    }
    0
}

/// `telescope mcp`: this user's running Telescope, over stdin and stdout.
pub fn run_stdio() -> i32 {
    use crate::single_instance::{connect_mcp, control_key_path, Address, McpRefused};
    let address = Address::current();
    let key = control_key_path(&telescope_core::config::config_path());
    let connect = move || match connect_mcp(&address, &key) {
        Ok(conn) => {
            let write = conn.try_clone().map_err(|e| e.to_string())?;
            Ok(Pipe { read: Box::new(conn), write: Box::new(write) })
        }
        Err(McpRefused::NotRunning) => Err("Telescope isn't running. Start Telescope, then try again.".to_string()),
        Err(McpRefused::Said(why)) => Err(why),
    };
    run(std::io::stdin().lock(), Box::new(std::io::stdout()), connect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::time::Duration;

    struct ChanReader(Receiver<Vec<u8>>, Vec<u8>);

    impl Read for ChanReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.1.is_empty() {
                match self.0.recv() {
                    Ok(bytes) => self.1 = bytes,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.1.len());
            buf[..n].copy_from_slice(&self.1[..n]);
            self.1.drain(..n);
            Ok(n)
        }
    }

    struct ChanWriter(Sender<Vec<u8>>);

    impl Write for ChanWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.send(buf.to_vec()).map_err(|_| std::io::ErrorKind::BrokenPipe)?;
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A Telescope that answers each request line with its method, and hangs up after answering `hang_up_after` requests.
    fn fake_telescope(hang_up_after: Option<usize>, seen: Sender<Value>) -> Pipe {
        let (to_bridge, from_server) = channel::<Vec<u8>>();
        let (to_server, from_bridge) = channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut answered = 0;
            for line in std::io::BufReader::new(ChanReader(from_bridge, Vec::new())).lines().map_while(Result::ok) {
                let message: Value = serde_json::from_str(&line).unwrap();
                let _ = seen.send(message.clone());
                if let Some(id) = message.get("id") {
                    if hang_up_after == Some(answered) {
                        return;
                    }
                    answered += 1;
                    let _ = to_bridge.send(format!("{}\n", json!({"jsonrpc": "2.0", "id": id, "result": {"echo": message["method"]}})).into_bytes());
                }
            }
        });
        Pipe { read: Box::new(ChanReader(from_server, Vec::new())), write: Box::new(ChanWriter(to_server)) }
    }

    fn lines(input: &[Value]) -> Cursor<String> {
        Cursor::new(input.iter().map(|m| format!("{m}\n")).collect())
    }

    fn collect(rx: &Receiver<Vec<u8>>, n: usize) -> Vec<Value> {
        (0..n).map(|_| serde_json::from_slice(rx.recv_timeout(Duration::from_secs(5)).expect("an answer").trim_ascii_end()).unwrap()).collect()
    }

    #[test]
    fn requests_go_to_telescope_and_its_answers_come_back_and_notifications_get_none() {
        let (seen_tx, seen) = channel();
        let (out_tx, out) = channel();
        let mut pipe = Some(fake_telescope(None, seen_tx));
        let input = lines(&[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}), json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                            json!({"jsonrpc": "2.0", "id": "b", "method": "ping"})]);
        assert_eq!(run(input, Box::new(ChanWriter(out_tx)), || pipe.take().ok_or_else(|| "connected twice".to_string())), 0);
        let answers = collect(&out, 2);
        assert!(answers.contains(&json!({"jsonrpc": "2.0", "id": 1, "result": {"echo": "tools/list"}})), "{answers:?}");
        assert!(answers.iter().any(|a| a["id"] == json!("b")));
        let forwarded: Vec<Value> = (0..3).map(|_| seen.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert_eq!(forwarded[1]["method"], json!("notifications/initialized"), "notifications are passed on too");
        assert!(out.recv_timeout(Duration::from_millis(100)).is_err(), "and get no answer");
    }

    #[test]
    fn with_telescope_not_running_the_bridge_still_answers_and_says_why() {
        let (out_tx, out) = channel();
        let why = "Telescope isn't running. Start Telescope, then try again.";
        let input = lines(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "status"}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 5, "method": "resources/list"}),
        ]);
        run(input, Box::new(ChanWriter(out_tx)), || Err(why.to_string()));
        let answers = collect(&out, 5);
        assert_eq!(answers[0]["result"]["protocolVersion"], json!("2025-03-26"));
        assert_eq!(answers[0]["result"]["instructions"], json!(why));
        assert_eq!(answers[1]["result"]["tools"], json!(crate::mcp::tools()), "the tools are offered all the same");
        assert_eq!((answers[2]["result"]["isError"].clone(), answers[2]["result"]["content"][0]["text"].clone()), (json!(true), json!(why)));
        assert_eq!(answers[3]["result"], json!({}));
        assert_eq!(answers[4]["error"]["message"], json!(why));
    }

    #[test]
    fn it_connects_again_when_telescope_comes_up_later_without_a_restart() {
        let (seen_tx, _seen) = channel();
        let (out_tx, out) = channel();
        let mut tries = 0;
        let mut pipe = Some(fake_telescope(None, seen_tx));
        let input = lines(&[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "status"}}),
                            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})]);
        run(input, Box::new(ChanWriter(out_tx)), || {
            tries += 1;
            if tries == 1 { Err("MCP is off".to_string()) } else { pipe.take().ok_or_else(|| "no".to_string()) }
        });
        let answers = collect(&out, 2);
        assert_eq!(answers[0]["result"]["isError"], json!(true));
        assert_eq!(answers[1]["result"], json!({"echo": "tools/list"}));
    }

    #[test]
    fn a_request_telescope_never_answered_gets_an_error_when_it_hangs_up_and_the_next_one_reconnects() {
        let (seen_tx, _seen) = channel();
        let (out_tx, out) = channel();
        let mut pipes = vec![fake_telescope(None, seen_tx.clone()), fake_telescope(Some(0), seen_tx)];
        // Both requests go in before the second connection is made, so feed them from a pipe that waits for the first answer
        let (feed_tx, feed_rx) = channel::<Vec<u8>>();
        let input = std::io::BufReader::new(ChanReader(feed_rx, Vec::new()));
        let finished = std::thread::spawn(move || run(input, Box::new(ChanWriter(out_tx)), move || pipes.pop().ok_or_else(|| "no more".to_string())));
        let send = |m: Value| feed_tx.send(format!("{m}\n").into_bytes()).unwrap();
        // pipes.pop() hands out the hanging-up one first
        send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "measure"}}));
        let lost = collect(&out, 1);
        assert_eq!(lost[0]["id"], json!(1));
        assert!(lost[0]["error"]["message"].as_str().unwrap().contains("closed the connection"), "{}", lost[0]);
        send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        assert_eq!(collect(&out, 1)[0]["result"], json!({"echo": "tools/list"}));
        drop(feed_tx);
        assert_eq!(finished.join().unwrap(), 0);
    }

    #[test]
    fn a_line_without_a_newline_is_skipped_as_it_comes_in_and_never_kept_whole() {
        // Longer than the cap, with no newline in it: the reader never holds more than the cap
        let long = std::io::repeat(b'a').take(3 * MAX_LINE as u64);
        let mut input = std::io::BufReader::with_capacity(8192, long);
        assert!(matches!(read_line(&mut input, MAX_LINE), Ok(Line::TooLong)));
        assert!(matches!(read_line(&mut input, MAX_LINE), Ok(Line::End)), "and what follows is the end, not the rest of the line");
    }

    #[test]
    fn an_oversize_message_is_refused_and_the_next_one_is_still_served() {
        let (out_tx, out) = channel();
        let mut input = format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}\n", "x".repeat(MAX_LINE + 10));
        input.push_str("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n");
        run(Cursor::new(input), Box::new(ChanWriter(out_tx)), || Err("off".into()));
        let answers = collect(&out, 2);
        assert_eq!(answers[0]["error"]["code"], json!(-32600));
        assert!(answers[0]["error"]["message"].as_str().unwrap().contains("longer than 1 MiB"));
        assert_eq!((answers[1]["id"].clone(), answers[1]["result"].clone()), (json!(2), json!({})), "the next message is answered as usual");
    }

    #[test]
    fn a_line_right_at_the_cap_is_taken_and_a_last_line_without_a_newline_too() {
        let mut input = Cursor::new(format!("{}\r\nlast", "a".repeat(MAX_LINE - 1)));
        assert!(matches!(read_line(&mut input, MAX_LINE), Ok(Line::Text(t)) if t.len() == MAX_LINE - 1), "a carriage return is not part of the message");
        assert!(matches!(read_line(&mut input, MAX_LINE), Ok(Line::Text(t)) if t == "last"));
        assert!(matches!(read_line(&mut input, MAX_LINE), Ok(Line::End)));
    }

    #[test]
    fn something_that_is_not_json_is_answered_with_a_parse_error() {
        let (out_tx, out) = channel();
        run(Cursor::new("{nope\n\n".to_string()), Box::new(ChanWriter(out_tx)), || Err("off".into()));
        assert_eq!(collect(&out, 1)[0]["error"]["code"], json!(-32700));
    }
}
