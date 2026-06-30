// 內嵌本地橋：127.0.0.1:3710 的 HTTP 服務。
//   ① 本地文件能力（列目錄/打開/讀取/複製/移動/保存）——線上門戶頁 fetch 即可，無需 Node、無需 Tauri IPC。
//   ② 原生窗口控制（拖動/最小化/最大化/隱藏/任務欄角標/瀏覽器開鏈接/新窗口開應用/檢查更新）。
// 之所以全走這條 HTTP 橋而非 Tauri IPC：主視窗加載的是「遠端」頁面（workers.dev），遠端頁拿不到 IPC。
// 瀏覽器允許 https 頁 fetch http://127.0.0.1（localhost 視為安全來源），故此橋對線上頁可用。
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use tiny_http::{Header, Method, Response, Server};

const PORTAL_BASE: &str = "https://huali-structure-app.qiaoyuhua2002.workers.dev";
// 橋能力清單：門戶頁 fetch /ping 看到 "win" 才啟用自繪頂欄（舊版桌面殼沒有→仍用系統標題欄，避免雙標題欄）。
const BRIDGE_CAPS: &[&str] = &["file", "win", "badge", "child", "update"];

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

// ───────── 原生窗口控制（都派到主線程執行，Windows 上窗口操作必須在事件循環線程）─────────

/// 對指定窗口執行一個操作（默認 main）。在主線程上跑，避免跨線程窗口操作崩潰。
fn on_window<F>(app: &AppHandle, label: &str, f: F)
where F: FnOnce(&tauri::WebviewWindow) + Send + 'static {
    let app2 = app.clone();
    let label = label.to_string();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = app2.get_webview_window(&label) { f(&w); }
    });
}

/// 讀窗口是否最大化（需返回值 → 用 channel 從主線程取回）。
fn is_maximized(app: &AppHandle, label: &str) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    let app2 = app.clone();
    let label = label.to_string();
    let _ = app.run_on_main_thread(move || {
        let v = app2.get_webview_window(&label).and_then(|w| w.is_maximized().ok()).unwrap_or(false);
        let _ = tx.send(v);
    });
    rx.recv_timeout(std::time::Duration::from_millis(600)).unwrap_or(false)
}

/// 任務欄角標：有新動態時顯示（Windows = 任務欄圖標角標數字 / macOS = Dock 紅點數）。on=false 清除。
fn set_badge(app: &AppHandle, on: bool) {
    on_window(app, "main", move |w| {
        let _ = w.set_badge_count(if on { Some(1) } else { None });
    });
}

/// 新窗口打開一個預設應用（route 如 /pm2）。已開則聚焦。子窗用系統標題欄（?dsk=child 讓網頁不畫自繪頂欄）。
fn open_child(app: &AppHandle, route: &str) {
    let label = format!("app{}", route.replace(['/', '-'], "_")); // 合法且穩定的窗口 label
    if let Some(w) = app.get_webview_window(&label) {
        on_window(app, &label, |w2| { let _ = w2.show(); let _ = w2.unminimize(); let _ = w2.set_focus(); });
        let _ = w;
        return;
    }
    let sep = if route.contains('?') { '&' } else { '?' };
    let full = format!("{}{}{}dsk=child", PORTAL_BASE, route, sep);
    if let Ok(url) = full.parse::<tauri::Url>() {
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            let _ = tauri::WebviewWindowBuilder::new(&app2, &label, tauri::WebviewUrl::External(url))
                .title("華利建機處")
                .inner_size(1280.0, 820.0)
                .min_inner_size(900.0, 600.0)
                .center()
                .build();
        });
    }
}

/// 供托盤菜單調用：在新窗口打開預設應用。
pub fn open_app_window(app: &AppHandle, route: &str) { open_child(app, route); }

// ───────── 自動更新（Rust 驅動：遠端頁拿不到 updater JS API，故由橋觸發）─────────

/// 檢查更新：返回 {available, version}。同步包裝異步 updater。
fn update_check(app: &AppHandle) -> Value {
    use tauri_plugin_updater::UpdaterExt;
    let app2 = app.clone();
    tauri::async_runtime::block_on(async move {
        match app2.updater() {
            Ok(u) => match u.check().await {
                Ok(Some(up)) => json!({"ok": true, "available": true, "version": up.version, "notes": up.body}),
                Ok(None) => json!({"ok": true, "available": false}),
                Err(e) => json!({"ok": false, "available": false, "msg": e.to_string()}),
            },
            Err(e) => json!({"ok": false, "available": false, "msg": e.to_string()}),
        }
    })
}

/// 下載並安裝更新，完成後重啟。失敗返回錯誤。
fn update_apply(app: &AppHandle) -> Value {
    use tauri_plugin_updater::UpdaterExt;
    let app2 = app.clone();
    let r: Result<bool, String> = tauri::async_runtime::block_on(async move {
        let updater = app2.updater().map_err(|e| e.to_string())?;
        let maybe = updater.check().await.map_err(|e| e.to_string())?;
        let Some(update) = maybe else { return Ok(false); };
        update.download_and_install(|_chunk, _total| {}, || {}).await.map_err(|e| e.to_string())?;
        Ok(true)
    });
    match r {
        Ok(true) => { app.restart(); }
        Ok(false) => json!({"ok": true, "applied": false, "msg": "已是最新版"}),
        Err(e) => json!({"ok": false, "msg": e}),
    }
}

// ───────── 本地文件能力（原有，保持不變）─────────

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

fn body_json(req: &mut tiny_http::Request) -> Value {
    let mut s = String::new();
    let _ = req.as_reader().read_to_string(&mut s);
    serde_json::from_str(&s).unwrap_or(json!({}))
}

pub fn start_bridge(app: AppHandle) {
    std::thread::spawn(move || {
        let server = match Server::http("127.0.0.1:3710") { Ok(s) => s, Err(_) => return };
        for mut req in server.incoming_requests() {
            if *req.method() == Method::Options { let _ = req.respond(json_resp(json!({"ok": true}))); continue; }
            let url = req.url().to_string();
            let (path, query) = url.split_once('?').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((url.clone(), String::new()));
            let q = parse_query(&query);
            let resp: Resp = match path.as_str() {
                "/ping" => json_resp(json!({"ok": true, "bridge": "huali-portal-embedded", "version": env!("CARGO_PKG_VERSION"), "caps": BRIDGE_CAPS})),

                // ── 原生窗口控制 ──
                "/win/start-drag" => { on_window(&app, q.get("win").map(|s| s.as_str()).unwrap_or("main"), |w| { let _ = w.start_dragging(); }); json_resp(json!({"ok": true})) }
                "/win/minimize" => { on_window(&app, "main", |w| { let _ = w.minimize(); }); json_resp(json!({"ok": true})) }
                "/win/toggle-maximize" => {
                    let max = is_maximized(&app, "main");
                    on_window(&app, "main", move |w| { let _ = if max { w.unmaximize() } else { w.maximize() }; });
                    json_resp(json!({"ok": true, "maximized": !max}))
                }
                "/win/is-maximized" => json_resp(json!({"ok": true, "maximized": is_maximized(&app, "main")})),
                "/win/hide" => { on_window(&app, "main", |w| { let _ = w.hide(); }); json_resp(json!({"ok": true})) }
                "/win/badge" => { set_badge(&app, q.get("on").map(|s| s == "1" || s == "true").unwrap_or(false)); json_resp(json!({"ok": true})) }

                // ── 在系統默認瀏覽器打開外部鏈接（比遠端 IPC 的 opener 可靠）──
                "/win/open-external" => {
                    let target = if *req.method() == Method::Post { body_json(&mut req).get("url").and_then(|x| x.as_str()).map(|s| s.to_string()) } else { q.get("url").cloned() };
                    match target {
                        Some(u) if u.starts_with("http://") || u.starts_with("https://") => {
                            let ok = app.opener().open_url(&u, None::<&str>).is_ok();
                            json_resp(json!({"ok": ok}))
                        }
                        _ => json_resp(json!({"ok": false, "msg": "need http(s) url"})),
                    }
                }

                // ── 新窗口打開預設應用 ──
                "/win/open-window" => {
                    let route = if *req.method() == Method::Post { body_json(&mut req).get("route").and_then(|x| x.as_str()).unwrap_or("/").to_string() } else { q.get("route").cloned().unwrap_or_else(|| "/".into()) };
                    open_child(&app, &route);
                    json_resp(json!({"ok": true}))
                }

                // ── 自動更新 ──
                "/win/update/check" => json_resp(update_check(&app)),
                "/win/update/apply" => json_resp(update_apply(&app)),

                // ── 本地文件能力（原有）──
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
                    let v = body_json(&mut req);
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
