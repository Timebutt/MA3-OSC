//! Request/reply channel to the MA3-OSC plugin, used by the MCP server.
//!
//! grandMA3 has no request/reply OSC, so the channel is built from what MA has:
//!  * request: OSC `/cmd SetGlobalVariable "ma3OscRpc" "<id>:<base64>"`. The plugin
//!    polls the variable (10 Hz), clears it and runs the request.
//!  * reply: the plugin sends `/ma3osc/rpc <id> <index> <count> <base64 chunk>`.
//!
//! Limits measured on grandMA3 2.5.1.1: MA reads at most 2048 bytes of an incoming
//! OSC packet (a longer `/cmd` is cut short and still answers "OK") and drops any
//! outgoing packet over 2048 bytes. So a request must fit in one packet, a reply is
//! split into chunks, and a missing chunk shows up as a timeout. Requests are
//! read-only, so a timed-out request is simply sent again under a new id.

use anyhow::{Result, anyhow, bail};
use rosc::OscType;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// OSC address of the plugin's reply chunks.
pub const REPLY_ADDR: &str = "/ma3osc/rpc";
/// MA global variable the plugin reads requests from.
pub const REQUEST_VAR: &str = "ma3OscRpc";
/// Must equal RPC_PROTOCOL in plugin/ma3-osc.lua.
pub const PROTOCOL: i64 = 1;
/// Longest base64 request. With the command around it, the OSC packet stays
/// well below the 2048 bytes MA reads.
const MAX_REQUEST_B64: usize = 1800;
/// Reply chunks the plugin may send (it sends 64 at most).
const MAX_CHUNKS: usize = 64;
const ATTEMPTS: usize = 2;

pub type OscSend = Box<dyn FnMut(&str, Vec<OscType>) + Send>;

/// One request in flight at a time: the plugin has a single request variable.
pub struct Broker {
    send: Mutex<OscSend>,
    cmd_addr: String,
    timeout: Duration,
    next_id: AtomicU64,
    /// Held for a whole call, so requests never overwrite each other in MA.
    call: Mutex<()>,
    reply: Mutex<Reply>,
    arrived: Condvar,
}

#[derive(Default)]
struct Reply {
    id: Option<String>,
    parts: Vec<Option<String>>,
    done: Option<String>,
}

impl Broker {
    /// `prefix` is the OSC prefix of MA's OSC line (config `ma.prefix`).
    pub fn new(prefix: &str, timeout: Duration, send: OscSend) -> Self {
        Broker {
            send: Mutex::new(send),
            cmd_addr: format!("{prefix}/cmd"),
            timeout,
            next_id: AtomicU64::new(1),
            call: Mutex::new(()),
            reply: Mutex::new(Reply::default()),
            arrived: Condvar::new(),
        }
    }

    /// Hands a `/ma3osc/rpc` message from MA to the waiting call.
    pub fn deliver(&self, args: &[OscType]) {
        let (Some(OscType::String(id)), Some(OscType::Int(index)), Some(OscType::Int(count)), Some(OscType::String(chunk))) =
            (args.first(), args.get(1), args.get(2), args.get(3))
        else {
            tracing::debug!(?args, "malformed reply from the MA plugin");
            return;
        };
        let (Ok(index), Ok(count)) = (usize::try_from(*index), usize::try_from(*count)) else { return };
        if !(1..=MAX_CHUNKS).contains(&count) || !(1..=count).contains(&index) {
            return;
        }
        let mut r = lock(&self.reply);
        if r.id.as_deref() != Some(id.as_str()) || r.done.is_some() {
            return; // late reply to an earlier attempt
        }
        if r.parts.len() != count {
            r.parts = vec![None; count];
        }
        r.parts[index - 1] = Some(chunk.clone());
        if r.parts.iter().all(Option::is_some) {
            r.done = Some(r.parts.iter().flatten().map(String::as_str).collect());
        }
        self.arrived.notify_all(); // every chunk restarts the timeout
    }

    /// Runs `tool` in the plugin and returns its result, or MA's error text.
    pub fn call(&self, tool: &str, args: &[(&str, String)]) -> Result<Value> {
        let text = request_text(tool, args)?;
        let body = b64encode(text.as_bytes());
        if body.len() > MAX_REQUEST_B64 {
            bail!("request too long for grandMA3 ({} base64 characters, at most {MAX_REQUEST_B64})", body.len());
        }
        let _one_at_a_time = lock(&self.call);
        for attempt in 1..=ATTEMPTS {
            let id = format!("r{}", self.next_id.fetch_add(1, Ordering::Relaxed));
            *lock(&self.reply) = Reply { id: Some(id.clone()), ..Reply::default() };
            let cmd = format!("SetGlobalVariable \"{REQUEST_VAR}\" \"{id}:{body}\"");
            let osc_args = vec![OscType::String(cmd)];
            crate::monitor::get().osc(crate::monitor::Dir::OscOut, &self.cmd_addr, &osc_args);
            (lock(&self.send))(&self.cmd_addr, osc_args);

            if let Some(reply) = self.wait() {
                return parse_reply(&reply);
            }
            tracing::debug!(tool, attempt, "no complete reply from the MA plugin");
        }
        *lock(&self.reply) = Reply::default();
        bail!(
            "the MA3-OSC plugin did not answer completely (no reply for {:?}, twice). Check that it is running in grandMA3 and \
             up to date (import plugin/ma3-osc.xml again and restart it)",
            self.timeout
        )
    }

    /// Waits until the reply is complete. The timeout counts from the last chunk,
    /// so a long reply (MA sends about one chunk per 30 ms) never runs out of time
    /// while it is still arriving.
    fn wait(&self) -> Option<String> {
        let mut deadline = Instant::now() + self.timeout;
        let mut received = 0;
        let mut r = lock(&self.reply);
        loop {
            if let Some(done) = r.done.take() {
                r.id = None;
                return Some(done);
            }
            let now = r.parts.iter().filter(|p| p.is_some()).count();
            if now > received {
                received = now;
                deadline = Instant::now() + self.timeout;
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            r = self.arrived.wait_timeout(r, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The tool name, then one `key=value` line per argument.
fn request_text(tool: &str, args: &[(&str, String)]) -> Result<String> {
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if !ok(tool) {
        bail!("invalid request name '{tool}'");
    }
    let mut text = tool.to_string();
    for (k, v) in args {
        if !ok(k) || v.contains(['\n', '\r']) {
            bail!("invalid request argument '{k}'");
        }
        text.push_str(&format!("\n{k}={v}"));
    }
    Ok(text)
}

fn parse_reply(b64: &str) -> Result<Value> {
    let bytes = b64decode(b64).ok_or_else(|| anyhow!("the MA plugin sent an unreadable reply"))?;
    let v: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|e| anyhow!("the MA plugin sent invalid JSON: {e}"))?;
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        Some(false) => bail!("grandMA3: {}", v.get("error").and_then(Value::as_str).unwrap_or("unknown error")),
        None => bail!("the MA plugin sent a reply without \"ok\""),
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64decode(s: &str) -> Option<Vec<u8>> {
    let index: HashMap<u8, u32> = B64.iter().enumerate().map(|(i, c)| (*c, i as u32)).collect();
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.chunks(4) {
        if c.len() == 1 {
            return None;
        }
        let mut n = 0u32;
        for (i, b) in c.iter().enumerate() {
            n |= index.get(b)? << (18 - 6 * i);
        }
        out.extend_from_slice(&n.to_be_bytes()[1..c.len()]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::mpsc;

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"a", b"ab", b"abc", b"abcd", b"\xff\x00\x10 status\npage=1"] {
            assert_eq!(b64decode(&b64encode(data)).unwrap(), data);
        }
        assert_eq!(b64encode(b"status"), "c3RhdHVz");
        assert!(b64decode("a").is_none());
        assert!(b64decode("ab\"c").is_none());
    }

    #[test]
    fn request_text_rejects_line_breaks() {
        assert_eq!(request_text("pool", &[("pool", "groups".into())]).unwrap(), "pool\npool=groups");
        assert!(request_text("pool", &[("pool", "a\nb".into())]).is_err());
        assert!(request_text("x y", &[]).is_err());
    }

    /// A fake plugin: decodes each request and answers with `answer(id, text)`.
    fn broker_with(answer: impl Fn(&str, &str) -> Vec<Vec<OscType>> + Send + 'static) -> (Arc<Broker>, mpsc::Receiver<String>) {
        broker_with_delay(Duration::ZERO, answer)
    }

    /// Like broker_with, with a pause before each reply chunk (like MA's Cmd time).
    fn broker_with_delay(
        delay: Duration,
        answer: impl Fn(&str, &str) -> Vec<Vec<OscType>> + Send + 'static,
    ) -> (Arc<Broker>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<(String, String)>();
        let (seen_tx, seen_rx) = mpsc::channel();
        let broker = Arc::new(Broker::new(
            "",
            Duration::from_millis(200),
            Box::new(move |addr, args| {
                assert_eq!(addr, "/cmd");
                let Some(OscType::String(cmd)) = args.first() else { panic!() };
                let value = cmd.strip_prefix("SetGlobalVariable \"ma3OscRpc\" \"").unwrap().strip_suffix('"').unwrap();
                let (id, body) = value.split_once(':').unwrap();
                let text = String::from_utf8(b64decode(body).unwrap()).unwrap();
                tx.send((id.to_string(), text)).unwrap();
            }),
        ));
        let b = broker.clone();
        std::thread::spawn(move || {
            for (id, text) in rx {
                let _ = seen_tx.send(text.clone());
                for msg in answer(&id, &text) {
                    std::thread::sleep(delay);
                    b.deliver(&msg);
                }
            }
        });
        (broker, seen_rx)
    }

    fn chunks(id: &str, json: &str, size: usize) -> Vec<Vec<OscType>> {
        let data = b64encode(json.as_bytes());
        let parts: Vec<&str> = data.as_bytes().chunks(size).map(|c| std::str::from_utf8(c).unwrap()).collect();
        let n = parts.len() as i32;
        parts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                vec![OscType::String(id.into()), OscType::Int(i as i32 + 1), OscType::Int(n), OscType::String(c.to_string())]
            })
            .collect()
    }

    #[test]
    fn chunked_reply_is_reassembled_in_any_order() {
        let (b, seen) = broker_with(|id, _| {
            let mut c = chunks(id, r#"{"ok":true,"result":{"names":["a","b","c","d","e","f"]}}"#, 8);
            c.reverse();
            c
        });
        let v = b.call("pool", &[("pool", "groups".into())]).unwrap();
        assert_eq!(v["names"][5], "f");
        assert_eq!(seen.recv().unwrap(), "pool\npool=groups");
    }

    #[test]
    fn missing_chunk_is_retried_then_reported() {
        let calls = Arc::new(AtomicU64::new(0));
        let c = calls.clone();
        let (b, _) = broker_with(move |id, _| {
            let mut parts = chunks(id, r#"{"ok":true,"result":1}"#, 4);
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                parts.pop(); // first attempt loses its last chunk
            }
            parts
        });
        assert_eq!(b.call("status", &[]).unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let (b, _) = broker_with(|_, _| vec![]);
        let e = b.call("status", &[]).unwrap_err().to_string();
        assert!(e.contains("did not answer"), "{e}");
    }

    #[test]
    fn slow_long_reply_does_not_time_out() {
        // 80 ms per chunk: the whole reply takes far longer than the 200 ms timeout
        let (b, _) = broker_with_delay(Duration::from_millis(80), |id, _| {
            let c = chunks(id, &format!(r#"{{"ok":true,"result":"{}"}}"#, "x".repeat(60)), 8);
            assert!(c.len() > 8);
            c
        });
        assert_eq!(b.call("fixtures", &[]).unwrap(), "x".repeat(60));
    }

    #[test]
    fn late_reply_to_an_old_attempt_is_ignored() {
        let (b, _) = broker_with(|id, _| {
            let mut out = chunks("r999", r#"{"ok":true,"result":"stale"}"#, 100);
            out.extend(chunks(id, r#"{"ok":true,"result":"fresh"}"#, 100));
            out
        });
        assert_eq!(b.call("status", &[]).unwrap(), "fresh");
    }

    #[test]
    fn plugin_errors_and_oversized_requests_are_errors() {
        let (b, _) = broker_with(|id, _| chunks(id, r#"{"ok":false,"error":"page 7 does not exist"}"#, 100));
        assert_eq!(b.call("executors", &[]).unwrap_err().to_string(), "grandMA3: page 7 does not exist");
        assert!(b.call("x", &[("v", "a".repeat(2000))]).unwrap_err().to_string().contains("too long"));
    }
}
