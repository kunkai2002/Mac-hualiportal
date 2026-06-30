// 內嵌本地文件橋：在桌面殼後台起一個 127.0.0.1:3710 的 HTTP 服務，提供本地文件能力。
// 線上門戶頁面用 fetch(localhost:3710) 即可訪問本地文件（列目錄/打開/讀取/複製/移動），
// 無需用戶另裝 Node，也不依賴 Tauri IPC（遠程頁拿不到 IPC）。
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Response, Server};

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

type Resp = Response<std::io::Cursor<Vec<u8>>>;

fn json_resp(v: Value) -> Resp {
    Response::from_string(v.to_string())
        .with_header(header("Content-Type", "application/json; charset=utf-8"))
        .with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Access-Control-Allow-Methods", "GET, POST, OPTIONS"))
        .with_header(header("Access-Control-Allow-Headers", "Content-Type"))
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for pair in q.split('&') {
        if pair.is_empty() { continue; }
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        let dk = urlencoding::decode(k).map(|c| c.into_owned()).unwrap_or_else(|_| k.to_string());
        let dv = urlencoding::decode(v).map(|c| c.into_owned()).unwrap_or_else(|_| v.to_string());
        m.insert(dk, dv);
    }
    m
}

fn list_dir(path: &str, recursive: bool) -> Value {
    let mut dirs: Vec<Value> = vec![];
    let mut files: Vec<Value> = vec![];
    fn walk(dir: &Path, recursive: bool, dirs: &mut Vec<Value>, files: &mut Vec<Value>) {
        let rd = match fs::read_dir(dir) { Ok(r) => r, Err(_) => return };
        for e in rd.flatten() {
            let p = e.path();
            let md = match e.metadata() { Ok(m) => m, Err(_) => continue };
            let name = e.file_name().to_string_lossy().to_string();
            let ps = p.to_string_lossy().to_string();
            if md.is_dir() {
                dirs.push(json!({"name": name, "path": ps}));
                if recursive { walk(&p, true, dirs, files); }
            } else {
                let modified = md.modified().ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis().to_string()).unwrap_or_default();
                files.push(json!({"name": name, "path": ps, "size": md.len(), "modified": modified, "isDir": false}));
            }
        }
    }
    walk(Path::new(path), recursive, &mut dirs, &mut files);
    json!({"dirs": dirs, "files": files})
}

fn drives() -> Value {
    let mut v: Vec<Value> = vec![];
    #[cfg(target_os = "windows")]
    for c in b'A'..=b'Z' {
        let p = format!("{}:\\", c as char);
        if Path::new(&p).exists() { v.push(json!({"name": format!("{}:", c as char), "path": p})); }
    }
    json!({"drives": v})
}

fn open_native(path: &str, reveal: bool) -> bool {
    #[cfg(target_os = "windows")]
    {
        if reveal { Command::new("explorer").args(["/select,", path]).spawn().is_ok() }
        else { Command::new("cmd").args(["/c", "start", "", path]).spawn().is_ok() }
    }
    #[cfg(not(target_os = "windows"))]
    { let _ = (path, reveal); false }
}

fn mime_of(p: &str) -> &'static str {
    match p.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "pdf" => "application/pdf", "png" => "image/png", "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif", "webp" => "image/webp", "svg" => "image/svg+xml",
        "mp4" => "video/mp4", "webm" => "video/webm", "mp3" => "audio/mpeg",
        "txt" | "log" | "md" => "text/plain", "csv" => "text/csv", "json" => "application/json",
        _ => "application/octet-stream",
    }
}

pub fn start_bridge() {
    std::thread::spawn(move || {
        let server = match Server::http("127.0.0.1:3710") { Ok(s) => s, Err(_) => return };
        for mut req in server.incoming_requests() {
            if *req.method() == Method::Options { let _ = req.respond(json_resp(json!({"ok": true}))); continue; }
            let url = req.url().to_string();
            let (path, query) = url.split_once('?').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((url.clone(), String::new()));
            let q = parse_query(&query);
            let resp: Resp = match path.as_str() {
                "/ping" => json_resp(json!({"ok": true, "bridge": "huali-portal-embedded", "version": env!("CARGO_PKG_VERSION")})),
                "/drives" => json_resp(drives()),
                "/ls" => match q.get("path") {
                    Some(p) if !p.is_empty() => json_resp(list_dir(p, q.get("recursive").map(|s| s == "true").unwrap_or(false))),
                    _ => json_resp(drives()),
                },
                "/open" => json_resp(json!({"ok": q.get("path").map(|p| open_native(p, false)).unwrap_or(false)})),
                "/reveal" => json_resp(json!({"ok": q.get("path").map(|p| open_native(p, true)).unwrap_or(false)})),
                "/read" => match q.get("path") {
                    Some(p) if Path::new(p).exists() => match fs::read(p) {
                        Ok(bytes) => Response::from_data(bytes)
                            .with_header(header("Content-Type", mime_of(p)))
                            .with_header(header("Access-Control-Allow-Origin", "*")),
                        Err(e) => json_resp(json!({"ok": false, "msg": e.to_string()})),
                    },
                    _ => json_resp(json!({"ok": false, "msg": "not found"})),
                },
                "/copy" | "/move" => {
                    let mut body = String::new();
                    let _ = req.as_reader().read_to_string(&mut body);
                    let v: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                    let src = v.get("src").and_then(|x| x.as_str()).unwrap_or("");
                    let dst = v.get("dst").and_then(|x| x.as_str()).unwrap_or("");
                    if src.is_empty() || dst.is_empty() {
                        json_resp(json!({"ok": false, "msg": "need src & dst"}))
                    } else {
                        let name = Path::new(src).file_name().map(|n| n.to_os_string()).unwrap_or_default();
                        let target = Path::new(dst).join(&name);
                        let _ = fs::create_dir_all(dst);
                        let r = if path == "/copy" { fs::copy(src, &target).map(|_| ()) } else { fs::rename(src, &target) };
                        match r {
                            Ok(_) => json_resp(json!({"ok": true, "target": target.to_string_lossy()})),
                            Err(e) => json_resp(json!({"ok": false, "msg": e.to_string()})),
                        }
                    }
                },
                "/save" => match q.get("path") {
                    Some(p) if !p.is_empty() => {
                        let mut buf: Vec<u8> = Vec::new();
                        let _ = req.as_reader().read_to_end(&mut buf);
                        if let Some(dir) = Path::new(p).parent() { let _ = fs::create_dir_all(dir); }
                        match fs::write(p, &buf) {
                            Ok(_) => json_resp(json!({"ok": true, "path": p, "bytes": buf.len()})),
                            Err(e) => json_resp(json!({"ok": false, "msg": e.to_string()})),
                        }
                    }
                    _ => json_resp(json!({"ok": false, "msg": "need path"})),
                },
                _ => json_resp(json!({"ok": false, "msg": "unknown"})),
            };
            let _ = req.respond(resp);
        }
    });
}
