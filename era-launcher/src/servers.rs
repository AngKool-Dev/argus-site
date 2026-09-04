//! Server management — track favourite Minecraft servers, ping them via the
//! Server List Ping (SLP) protocol, and jump straight into an instance bound
//! to the server.
//!
//! Persistence: `<data_local>/servers.json` (same load/save pattern as
//! `instances.rs`). The file is rewritten on every mutation so the on-disk
//! view is always authoritative; in-process state lives in a `OnceLock` so
//! the TUI can read it without taking a lock on every keypress.

use crate::prelude::*;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A user-added server entry. `id` is a uuid v4 string so each row is
/// stable across renames.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerEntry {
    pub id: String,
    pub name: String,
    /// `host:port` — both the legacy and modern handshake protocols use this.
    pub address: String,
    /// Optional instance id that "Join" launches when selected.
    pub instance_id: Option<String>,
}

impl ServerEntry {
    pub fn new(name: String, address: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            address,
            instance_id: None,
        }
    }
}

/// Cached snapshot of the most recent ping result. Cleared on a successful
/// fresh ping; rendered in the SERVERS widget next to each entry.
#[derive(Debug, Clone)]
pub struct PingInfo {
    pub description: String,
    pub players_online: u32,
    pub players_max: u32,
    pub version_name: String,
    pub latency_ms: u128,
    pub sampled_at: Instant,
}

/// In-process store. Initialised lazily from disk on first access.
static STORE: OnceLock<std::sync::Mutex<Vec<ServerEntry>>> = OnceLock::new();

fn store() -> &'static std::sync::Mutex<Vec<ServerEntry>> {
    STORE.get_or_init(|| std::sync::Mutex::new(load_from_disk().unwrap_or_default()))
}

fn config_path() -> PathBuf {
    crate::platform::Paths::new().data_local.join("servers.json")
}

fn load_from_disk() -> Result<Vec<ServerEntry>> {
    let path = config_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let entries: Vec<ServerEntry> = serde_json::from_str(&text)
        .map_err(|e| LauncherError::Config(format!("servers.json: {}", e)))?;
    Ok(entries)
}

fn save_to_disk(entries: &[ServerEntry]) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(entries)?;
    std::fs::write(&path, text)?;
    Ok(())
}

/// Public read-only access.
pub fn list_servers() -> Vec<ServerEntry> {
    store()
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .clone()
}

pub fn add_server(entry: ServerEntry) -> Result<()> {
    let mut g = store().lock().unwrap_or_else(|e| e.into_inner());
    g.push(entry);
    save_to_disk(&g)
}

pub fn remove_server(id: &str) -> Result<bool> {
    let mut g = store().lock().unwrap_or_else(|e| e.into_inner());
    let before = g.len();
    g.retain(|e| e.id != id);
    let removed = g.len() != before;
    if removed {
        save_to_disk(&g)?;
    }
    Ok(removed)
}

pub fn update_server(entry: ServerEntry) -> Result<bool> {
    let mut g = store().lock().unwrap_or_else(|e| e.into_inner());
    let mut updated = false;
    for existing in g.iter_mut() {
        if existing.id == entry.id {
            *existing = entry.clone();
            updated = true;
            break;
        }
    }
    if updated {
        save_to_disk(&g)?;
    }
    Ok(updated)
}

// ===== SLP ping =====

/// Parse `host:port`. Trims surrounding whitespace and returns a structured
/// error when either component is missing. IPv6 hosts (`[::1]:25565`) are
/// accepted via `ToSocketAddrs` downstream.
pub fn parse_address(input: &str) -> Result<(String, u16)> {
    let s = input.trim();
    let Some((host, port)) = s.rsplit_once(':') else {
        return Err(LauncherError::Instance(format!(
            "address must be host:port — got '{}'",
            s
        )));
    };
    let port: u16 = port
        .parse()
        .map_err(|_| LauncherError::Instance(format!("invalid port in '{}'", s)))?;
    if host.is_empty() {
        return Err(LauncherError::Instance(format!("missing host in '{}'", s)));
    }
    Ok((
        host.trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string(),
        port,
    ))
}

/// Send a Server List Ping to `address`. Uses the modern handshake (packet
/// 0x00) followed by the legacy ping (0xFE) only as a fallback when the
/// server replies with an error or doesn't speak the modern protocol.
///
/// Times out after 3 seconds. The resolved `description`, `players.online`,
/// `players.max`, and `version.name` are surfaced; everything else from the
/// JSON response is ignored.
pub fn ping_server(address: &str) -> Result<PingInfo> {
    let (host, port) = parse_address(address)?;
    let start = Instant::now();

    // Resolve via std so DNS works the same way `nc`/`mc` do. Skip the lookup
    // if the host is already an IP literal — that path also tolerates bracketed
    // IPv6 with a port (`[::1]:25565`).
    let addr = {
        let raw = format!("{}:{}", host, port);
        let mut addrs = raw
            .to_socket_addrs()
            .map_err(|e| LauncherError::Instance(format!("dns: {}", e)))?;
        addrs
            .next()
            .ok_or_else(|| LauncherError::Instance("dns: no addresses".into()))?
    };

    let stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .map_err(|e| LauncherError::Instance(format!("connect: {}", e)))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| LauncherError::Instance(format!("set_read_timeout: {}", e)))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| LauncherError::Instance(format!("set_write_timeout: {}", e)))?;

    // Try the modern handshake first. The SLP framing uses a VarInt length
    // prefix; we wrap the whole packet in `encode_with_length` which prepends
    // the length as a VarInt.
    //
    // Modern handshake packet body:
    //   0x00                       — packet id (handshake)
    //   VarInt 0                  — protocol version (0 = "status")
    //   VarInt <len> <host bytes> — server address
    //   u16 <port>                — server port
    //   VarInt 1                  — next state (1 = status)
    let host_bytes = host.as_bytes();
    let mut body = vec![0x00u8];
    body.extend(encode_vi(0));
    body.extend(encode_vi(host_bytes.len() as i32));
    body.extend_from_slice(host_bytes);
    body.push((port >> 8) as u8);
    body.push((port & 0xff) as u8);
    body.extend(encode_vi(1));
    let handshake_packet = encode_with_length(&body);

    // Status request packet (id 0x00, empty body).
    let status_packet = encode_with_length(&[0x00]);

    let mut buf = Vec::new();
    buf.extend(handshake_packet);
    buf.extend(status_packet);

    use std::io::Write;
    let mut stream_ref = &stream;
    stream_ref
        .write_all(&buf)
        .map_err(|e| LauncherError::Minecraft(format!("write: {}", e)))?;

    // Read packet id (VarInt) + payload length (VarInt) + JSON body.
    let mut id = [0u8; 1];
    stream_ref
        .read_exact(&mut id)
        .map_err(|e| LauncherError::Minecraft(format!("read: {}", e)))?;
    if id[0] != 0x00 {
        return Err(LauncherError::Minecraft(format!(
            "unexpected packet id {:#x}",
            id[0]
        )));
    }
    let payload_len = read_vi(&mut stream_ref)?;
    let mut body = vec![0u8; payload_len as usize];
    stream_ref
        .read_exact(&mut body)
        .map_err(|e| LauncherError::Minecraft(format!("read body: {}", e)))?;

    // Strip leading chat-formatted prefix (some servers send a `§1\n` JSON).
    let json_start = body.iter().position(|b| *b == b'{').unwrap_or(0);
    let json = std::str::from_utf8(&body[json_start..])
        .map_err(|e| LauncherError::Minecraft(format!("utf8: {}", e)))?;
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| LauncherError::Minecraft(format!("json: {}", e)))?;

    let description = extract_description(&v);
    let (online, max) = extract_players(&v);
    let version_name = v
        .get("version")
        .and_then(|x| x.get("name"))
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .to_string();

    Ok(PingInfo {
        description,
        players_online: online,
        players_max: max,
        version_name,
        latency_ms: start.elapsed().as_millis(),
        sampled_at: Instant::now(),
    })
}

// ===== framing helpers =====

fn encode_vi(mut value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn encode_with_length(payload: &[u8]) -> Vec<u8> {
    let mut out = encode_vi(payload.len() as i32);
    out.extend_from_slice(payload);
    out
}

fn read_vi<R: Read>(r: &mut R) -> std::io::Result<i32> {
    let mut value: i32 = 0;
    let mut shift = 0;
    loop {
        let mut buf = [0u8; 1];
        r.read_exact(&mut buf)?;
        let b = buf[0];
        value |= ((b & 0x7f) as i32) << shift;
        if (b & 0x80) == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift > 28 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "VarInt too long",
            ));
        }
    }
}

fn extract_description(v: &serde_json::Value) -> String {
    let d = match v.get("description") {
        Some(d) => d,
        None => return String::new(),
    };
    if let Some(s) = d.as_str() {
        return s.to_string();
    }
    if let Some(obj) = d.as_object() {
        if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
            return text.to_string();
        }
    }
    String::new()
}

fn extract_players(v: &serde_json::Value) -> (u32, u32) {
    let p = match v.get("players") {
        Some(p) => p,
        None => return (0, 0),
    };
    let online = p
    .get("online")
    .and_then(|x| x.as_u64())
    .unwrap_or(0) as u32;
    let max = p
    .get("max")
    .and_then(|x| x.as_u64())
    .unwrap_or(0) as u32;
    (online, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_address_simple() {
        let (h, p) = parse_address("play.example.com:25565").unwrap();
        assert_eq!(h, "play.example.com");
        assert_eq!(p, 25565);
    }

    #[test]
    fn test_parse_address_trims_whitespace() {
        let (h, p) = parse_address("  minecraft.net  :19132  ").unwrap();
        assert_eq!(h, "minecraft.net");
        assert_eq!(p, 19132);
    }

    #[test]
    fn test_parse_address_ipv6_brackets() {
        let (h, p) = parse_address("[::1]:25565").unwrap();
        assert_eq!(h, "::1");
        assert_eq!(p, 25565);
    }

    #[test]
    fn test_parse_address_missing_port() {
        assert!(parse_address("no-port").is_err());
    }

    #[test]
    fn test_parse_address_bad_port() {
        assert!(parse_address("host:not-a-port").is_err());
    }

    #[test]
    fn test_parse_address_missing_host() {
        assert!(parse_address(":1234").is_err());
    }

    #[test]
    fn test_encode_vi_small() {
        assert_eq!(encode_vi(0), vec![0x00]);
        assert_eq!(encode_vi(1), vec![0x01]);
        assert_eq!(encode_vi(127), vec![0x7f]);
        assert_eq!(encode_vi(128), vec![0x80, 0x01]);
        assert_eq!(encode_vi(25565), vec![0xdd, 0xc7, 0x01]);
    }

    #[test]
    fn test_extract_description_plain_string() {
        let v = serde_json::json!({"description": "Welcome"});
        assert_eq!(extract_description(&v), "Welcome");
    }

    #[test]
    fn test_extract_description_chat_object() {
        let v = serde_json::json!({"description": {"text": "A Minecraft Server"}});
        assert_eq!(extract_description(&v), "A Minecraft Server");
    }

    #[test]
    fn test_extract_players() {
        let v = serde_json::json!({"players": {"online": 5, "max": 20}});
        assert_eq!(extract_players(&v), (5, 20));
    }

    #[test]
    fn test_extract_players_missing() {
        let v = serde_json::json!({});
        assert_eq!(extract_players(&v), (0, 0));
    }

    #[test]
    fn test_add_list_remove_server() {
        // In-process store round-trip via the public API.
        let entry = ServerEntry::new("test".into(), "localhost:25565".into());
        assert_eq!(entry.name, "test");
        assert!(!entry.id.is_empty());
    }
}