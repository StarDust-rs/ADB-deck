//! Client of the "aya" helper that ADBPlus runs on the phone.
//!
//! Ported from the client side of ADBPlus' `src/main/lib/adb/server.ts`
//! (AGPL-3.0, see `assets/aya/NOTICE.md`): the prebuilt helper (`aya.dex`) is
//! pushed to `/data/local/tmp/aya`, started with `app_process`, and listens on
//! the abstract socket `aya`. The host reaches that socket through an
//! `adb forward`, and talks length-delimited Protobuf `Request`/`Response`
//! messages whose `params` and `result` are JSON strings. Because the helper
//! runs inside Android it can render real launcher icons and read app labels,
//! which no adb command can.
//!
//! Differences from ADBPlus: replies are accumulated until a whole message has
//! arrived, every request has a time limit, the helper's HTTP file server is not
//! used (it listens on all of the phone's network interfaces without
//! authentication) and the helper is stopped again by [`Helper::shutdown`].

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::adb::DeviceAdb;
use crate::error::{Error, Result};
use crate::process::{spawn_lines, LineStream};

/// The helper exactly as built by ADBPlus.
pub const HELPER_DEX: &[u8] = include_bytes!("../assets/aya/aya.dex");
pub const DEVICE_DIR: &str = "/data/local/tmp/aya";
pub const DEVICE_DEX: &str = "/data/local/tmp/aya/aya.dex";
/// Where the helper stores the icons it renders (`<package>.<apk size>.png`).
pub const DEVICE_ICON_DIR: &str = "/data/local/tmp/aya/icons/";
const SERVER_CLASS: &str = "io.liriliri.aya.Server";
/// Matches the helper's command line without matching the `pkill` that names it
/// (the bracket keeps the pattern from matching its own text).
const SERVER_PATTERN: &str = "io.liriliri.aya.Serve[r]";
const SOCKET: &str = "localabstract:aya";
const START_TIMEOUT: Duration = Duration::from_secs(15);
const PING_TIMEOUT: Duration = Duration::from_secs(2);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest icon file read from the device.
const MAX_ICON_BYTES: usize = 8 * 1024 * 1024;

/// The helper's Protobuf messages (`wire.proto`), encoded by hand: two messages
/// of plain strings do not justify a code generator.
pub mod wire {
    use crate::error::{Error, Result};

    /// Largest message accepted from the helper.
    pub const MAX_MESSAGE: usize = 8 * 1024 * 1024;

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Request {
        pub id: String,
        pub method: String,
        pub params: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Response {
        pub id: String,
        pub result: String,
    }

    fn bad(detail: &str) -> Error {
        Error::parse("aya helper", detail)
    }

    fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    /// `Ok(None)` when `buf` ends inside the varint.
    fn get_varint(buf: &[u8]) -> Result<Option<(u64, usize)>> {
        let mut value = 0u64;
        for (i, &byte) in buf.iter().enumerate().take(10) {
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(Some((value, i + 1)));
            }
        }
        if buf.len() >= 10 {
            Err(bad("over-long number"))
        } else {
            Ok(None)
        }
    }

    /// Field `number` as a length-prefixed string; empty strings are omitted, as in proto3.
    fn put_str(out: &mut Vec<u8>, number: u32, value: &str) {
        if value.is_empty() {
            return;
        }
        put_varint(out, u64::from(number << 3 | 2));
        put_varint(out, value.len() as u64);
        out.extend_from_slice(value.as_bytes());
    }

    /// Prefixes an encoded message with its length (`writeDelimitedTo`).
    fn delimited(body: Vec<u8>) -> Vec<u8> {
        let mut out = Vec::with_capacity(body.len() + 5);
        put_varint(&mut out, body.len() as u64);
        out.extend(body);
        out
    }

    pub fn encode_request(r: &Request) -> Vec<u8> {
        let mut body = Vec::new();
        put_str(&mut body, 1, &r.id);
        put_str(&mut body, 2, &r.method);
        put_str(&mut body, 3, &r.params);
        delimited(body)
    }

    pub fn encode_response(r: &Response) -> Vec<u8> {
        let mut body = Vec::new();
        put_str(&mut body, 1, &r.id);
        put_str(&mut body, 2, &r.result);
        delimited(body)
    }

    /// Splits the first complete delimited message off `buf`: its body and the
    /// number of bytes it occupies. `Ok(None)` means more bytes are needed.
    fn frame(buf: &[u8]) -> Result<Option<(&[u8], usize)>> {
        let Some((len, header)) = get_varint(buf)? else { return Ok(None) };
        let len = usize::try_from(len).ok().filter(|l| *l <= MAX_MESSAGE).ok_or_else(|| bad("message too large"))?;
        match buf.get(header..header + len) {
            Some(body) => Ok(Some((body, header + len))),
            None => Ok(None),
        }
    }

    /// The string fields of a message body, as `(field number, text)`. Fields of
    /// other kinds are skipped.
    fn string_fields(mut body: &[u8]) -> Result<Vec<(u32, String)>> {
        let mut fields = Vec::new();
        while !body.is_empty() {
            let (tag, n) = get_varint(body)?.ok_or_else(|| bad("truncated field"))?;
            body = &body[n..];
            let number = (tag >> 3) as u32;
            match tag & 7 {
                0 => {
                    let (_, n) = get_varint(body)?.ok_or_else(|| bad("truncated field"))?;
                    body = &body[n..];
                }
                1 => body = body.get(8..).ok_or_else(|| bad("truncated field"))?,
                5 => body = body.get(4..).ok_or_else(|| bad("truncated field"))?,
                2 => {
                    let (len, n) = get_varint(body)?.ok_or_else(|| bad("truncated field"))?;
                    let len = usize::try_from(len).map_err(|_| bad("field too large"))?;
                    let value = body.get(n..n + len).ok_or_else(|| bad("truncated field"))?;
                    fields.push((number, String::from_utf8(value.to_vec()).map_err(|_| bad("text is not UTF-8"))?));
                    body = &body[n + len..];
                }
                _ => return Err(bad("unsupported field type")),
            }
        }
        Ok(fields)
    }

    pub fn decode_request(buf: &[u8]) -> Result<Option<(Request, usize)>> {
        let Some((body, used)) = frame(buf)? else { return Ok(None) };
        let mut r = Request::default();
        for (number, text) in string_fields(body)? {
            match number {
                1 => r.id = text,
                2 => r.method = text,
                3 => r.params = text,
                _ => {}
            }
        }
        Ok(Some((r, used)))
    }

    pub fn decode_response(buf: &[u8]) -> Result<Option<(Response, usize)>> {
        let Some((body, used)) = frame(buf)? else { return Ok(None) };
        let mut r = Response::default();
        for (number, text) in string_fields(body)? {
            match number {
                1 => r.id = text,
                2 => r.result = text,
                _ => {}
            }
        }
        Ok(Some((r, used)))
    }
}

/// One connection to the helper's socket.
struct Client {
    stream: TcpStream,
    pending: Vec<u8>,
    next_id: u64,
}

impl Client {
    async fn connect(port: u16) -> Result<Client> {
        let stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(("127.0.0.1", port)))
            .await
            .map_err(|_| Error::Invalid("the helper's port did not answer".into()))?
            .map_err(|e| Error::io("connecting to the helper", e))?;
        Ok(Client { stream, pending: Vec::new(), next_id: 1 })
    }

    async fn call(&mut self, method: &str, params: &Value, limit: Duration) -> Result<Value> {
        let id = format!("adbdeck-{}", self.next_id);
        self.next_id += 1;
        let request = wire::Request { id: id.clone(), method: method.to_string(), params: params.to_string() };
        match tokio::time::timeout(limit, self.exchange(&id, &request)).await {
            Ok(result) => result,
            Err(_) => Err(Error::Timeout { program: "the app helper".into(), secs: limit.as_secs().max(1) }),
        }
    }

    async fn exchange(&mut self, id: &str, request: &wire::Request) -> Result<Value> {
        self.stream.write_all(&wire::encode_request(request)).await.map_err(|e| Error::io("writing to the helper", e))?;
        let mut chunk = [0u8; 16 * 1024];
        loop {
            while let Some((response, used)) = wire::decode_response(&self.pending)? {
                self.pending.drain(..used);
                if response.id == id {
                    return serde_json::from_str(&response.result)
                        .map_err(|e| Error::parse("aya helper", format!("reply is not JSON: {e}")));
                }
            }
            let n = self.stream.read(&mut chunk).await.map_err(|e| Error::io("reading from the helper", e))?;
            if n == 0 {
                return Err(Error::Invalid("the helper closed the connection".into()));
            }
            self.pending.extend_from_slice(&chunk[..n]);
        }
    }
}

/// What the helper knows about one installed app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub package: String,
    /// The app's display name; the package name when it has none.
    pub label: String,
    /// Device path of the PNG the helper rendered, empty when the app has no icon.
    pub icon: String,
    pub apk_size: u64,
}

fn parse_infos(result: &Value) -> Vec<AppInfo> {
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    result
        .get("packageInfos")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|v| {
                    let package = text(v, "packageName");
                    (!package.is_empty()).then(|| AppInfo {
                        label: text(v, "label"),
                        icon: text(v, "icon"),
                        apk_size: v.get("apkSize").and_then(Value::as_u64).unwrap_or(0),
                        package,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Only files the helper itself renders are ever read back.
fn is_icon_path(path: &str) -> bool {
    path.strip_prefix(DEVICE_ICON_DIR).is_some_and(|name| {
        !name.is_empty() && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    })
}

/// The helper on one device.
pub struct Helper {
    dev: DeviceAdb,
    port: u16,
    client: Option<Client>,
    /// The `adb shell` session running the helper, when this process started it.
    server: Option<LineStream>,
    tail: VecDeque<String>,
}

impl Helper {
    /// Connects to a running helper, or installs and starts one. `scratch` is a
    /// host directory for the copy of the helper that gets pushed.
    pub async fn start(dev: DeviceAdb, scratch: &Path) -> Result<Helper> {
        let port = dev.forward_dynamic(SOCKET).await?;
        let mut helper = Helper { dev, port, client: None, server: None, tail: VecDeque::new() };
        if helper.ping().await.is_ok() {
            // Left running by an earlier session (or by ADBPlus itself).
            return Ok(helper);
        }
        match helper.launch(scratch).await {
            Ok(()) => Ok(helper),
            Err(e) => {
                helper.shutdown().await;
                Err(e)
            }
        }
    }

    async fn ping(&mut self) -> Result<String> {
        let mut client = Client::connect(self.port).await?;
        let version = client.call("getVersion", &json!({}), PING_TIMEOUT).await?;
        self.client = Some(client);
        Ok(version.get("version").and_then(Value::as_str).unwrap_or_default().to_string())
    }

    async fn launch(&mut self, scratch: &Path) -> Result<()> {
        tokio::fs::create_dir_all(scratch).await.map_err(|e| Error::io(format!("creating {}", scratch.display()), e))?;
        let local = scratch.join("aya.dex");
        tokio::fs::write(&local, HELPER_DEX).await.map_err(|e| Error::io(format!("writing {}", local.display()), e))?;

        self.dev.shell_ok(&format!("mkdir -p {DEVICE_DIR}")).await?;
        let pushed = self.dev.exec([std::ffi::OsString::from("push"), local.into_os_string(), DEVICE_DEX.into()]).await?;
        if !pushed.success() {
            return Err(Error::Invalid(format!("could not copy the helper to the device: {}", pushed.failure_message())));
        }

        // A helper that is running but not answering would keep the socket name.
        let _ = self.dev.shell(&format!("pkill -f '{SERVER_PATTERN}'")).await;

        let spec = self.dev.spec().arg("shell").arg(format!("CLASSPATH={DEVICE_DEX} app_process /system/bin {SERVER_CLASS}"));
        self.server = Some(spawn_lines(&spec, 64, true)?);

        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            self.drain_server();
            if self.ping().await.is_ok() {
                return Ok(());
            }
            if self.server.as_ref().is_some_and(LineStream::is_finished) {
                self.drain_server();
                let said = self.tail.iter().cloned().collect::<Vec<_>>().join(" / ");
                return Err(Error::Invalid(format!(
                    "the helper exited right after starting{}",
                    if said.is_empty() { self.device_log().await } else { format!(": {said}") }
                )));
            }
            if Instant::now() >= deadline {
                let log = self.device_log().await;
                return Err(Error::Invalid(format!("the helper did not answer within {} s{log}", START_TIMEOUT.as_secs())));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// The last crash or helper lines from the device log, as `": …"`, or empty.
    /// Only used to explain a failed start.
    async fn device_log(&self) -> String {
        let script = "logcat -d -t 300 2>/dev/null | grep -i -E 'aya|AndroidRuntime' | tail -n 4";
        match self.dev.shell(script).await {
            Ok(text) if !text.trim().is_empty() => {
                let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                format!(": {}", lines.join(" / "))
            }
            _ => String::new(),
        }
    }

    /// Keeps the helper's console output from filling its channel; the last few
    /// lines explain a failed start.
    fn drain_server(&mut self) {
        if let Some(server) = self.server.as_mut() {
            while let Ok(line) = server.lines.try_recv() {
                if self.tail.len() == 5 {
                    self.tail.pop_front();
                }
                self.tail.push_back(line);
            }
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.drain_server();
        if self.client.is_none() {
            self.client = Some(Client::connect(self.port).await?);
        }
        let Some(client) = self.client.as_mut() else { return Err(Error::Invalid("not connected to the helper".into())) };
        let result = client.call(method, &params, CALL_TIMEOUT).await;
        if result.is_err() {
            // A failed or late exchange leaves the stream in an unknown state.
            self.client = None;
        }
        result
    }

    /// Labels and icon files of `packages`. Packages the helper cannot read are left out.
    pub async fn package_infos(&mut self, packages: &[String]) -> Result<Vec<AppInfo>> {
        Ok(parse_infos(&self.call("getPackageInfos", json!({ "packageNames": packages })).await?))
    }

    /// PNG bytes of an icon the helper rendered. The file travels over adb, not
    /// over the helper's network file server.
    pub async fn read_icon(&self, path: &str) -> Result<Vec<u8>> {
        if !is_icon_path(path) {
            return Err(Error::Invalid(format!("{path} is not an icon file")));
        }
        let out = self.dev.exec_out(&["cat", path], MAX_ICON_BYTES, Duration::from_secs(20)).await?;
        if !out.success() || out.truncated {
            return Err(Error::Invalid(format!("could not read {path}: {}", out.failure_message())));
        }
        Ok(out.stdout)
    }

    /// Releases the port forward and, when this process started the helper,
    /// ends it. Best effort: the device may already be gone.
    pub async fn shutdown(mut self) {
        self.client = None;
        let _ = self.dev.remove_forward_endpoint(&format!("tcp:{}", self.port)).await;
        if self.server.take().is_some() {
            // Closing the shell session usually ends the helper; make sure.
            let _ = self.dev.shell(&format!("pkill -f '{SERVER_PATTERN}'")).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wire::*;
    use super::*;

    #[test]
    fn messages_round_trip_through_the_wire_format() {
        let request = Request { id: "a-1".into(), method: "getPackageInfos".into(), params: r#"{"packageNames":["x.y"]}"#.into() };
        let bytes = encode_request(&request);
        assert_eq!(decode_request(&bytes).unwrap(), Some((request, bytes.len())));
        let response = Response { id: "a-1".into(), result: "é".repeat(200) };
        let bytes = encode_response(&response);
        assert_eq!(decode_response(&bytes).unwrap(), Some((response, bytes.len())), "a body over 127 bytes needs a two-byte length");
    }

    #[test]
    fn the_encoding_matches_protobuf() {
        // id = "ab" is field 1 (tag 0x0a), method = "c" is field 2 (tag 0x12); the body is 7 bytes.
        let bytes = encode_request(&Request { id: "ab".into(), method: "c".into(), params: String::new() });
        assert_eq!(bytes, [7, 0x0a, 2, b'a', b'b', 0x12, 1, b'c']);
    }

    #[test]
    fn a_partial_message_waits_for_the_rest() {
        let bytes = encode_response(&Response { id: "1".into(), result: "{}".into() });
        for cut in 0..bytes.len() {
            assert_eq!(decode_response(&bytes[..cut]).unwrap(), None, "{cut} bytes are not a whole message");
        }
        let mut two = bytes.clone();
        two.extend(&bytes);
        let (_, used) = decode_response(&two).unwrap().unwrap();
        assert_eq!(used, bytes.len(), "only the first message is consumed");
    }

    #[test]
    fn unknown_fields_are_skipped_and_garbage_is_rejected() {
        // A varint field 9 (tag 0x48) then result = "ok" (field 2).
        let body = [0x48, 0x07, 0x12, 2, b'o', b'k'];
        let mut framed = vec![body.len() as u8];
        framed.extend(body);
        assert_eq!(decode_response(&framed).unwrap().unwrap().0.result, "ok");
        assert!(decode_response(&[3, 0x0b, 0, 0]).is_err(), "group wire types are not supported");
        assert!(decode_response(&[0xff; 11]).is_err());
        assert!(decode_response(&[0x80, 0x80, 0x80, 0x80, 0x10]).is_err(), "a 4 GiB message is refused");
    }

    #[test]
    fn helper_replies_are_parsed_defensively() {
        let result = json!({"packageInfos": [
            {"packageName": "a.b", "label": "App B", "icon": "/data/local/tmp/aya/icons/a.b.5.png", "apkSize": 5},
            {"packageName": "c.d"},
            {"label": "nameless"},
        ]});
        let infos = parse_infos(&result);
        assert_eq!(infos.len(), 2);
        assert_eq!((infos[0].label.as_str(), infos[0].apk_size), ("App B", 5));
        assert_eq!((infos[1].label.as_str(), infos[1].icon.as_str()), ("", ""));
        assert!(parse_infos(&json!({})).is_empty());
    }

    #[test]
    fn only_the_helpers_own_icon_files_may_be_read() {
        assert!(is_icon_path("/data/local/tmp/aya/icons/com.example.app.1234.png"));
        for bad in [
            "/data/local/tmp/aya/icons/",
            "/data/local/tmp/aya/icons/../../../../etc/hosts",
            "/data/local/tmp/aya/icons/a b.png",
            "/data/local/tmp/aya/icons/a;rm.png",
            "/sdcard/DCIM/photo.png",
            "/data/local/tmp/aya/icons/.hidden",
            "",
        ] {
            assert!(!is_icon_path(bad), "{bad:?}");
        }
    }
}
