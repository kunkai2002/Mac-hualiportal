// 內嵌本地橋：127.0.0.1:3710 的 HTTP 服務。
//   ① 本地文件能力（列目錄/打開/讀取/複製/移動/保存）——線上門戶頁 fetch 即可，無需 Node、無需 Tauri IPC。
//   ② 原生窗口控制（拖動/最小化/最大化/隱藏/任務欄角標/瀏覽器開鏈接/新窗口開應用/檢查更新）。
// 之所以全走這條 HTTP 橋而非 Tauri IPC：主視窗加載的是「遠端」頁面（workers.dev），遠端頁拿不到 IPC。
// 瀏覽器允許 https 頁 fetch http://127.0.0.1（localhost 視為安全來源），故此橋對線上頁可用。
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use tiny_http::{Header, Method, Response, Server};

const PORTAL_BASE: &str = "https://huali-structure-app.qiaoyuhua2002.workers.dev";
// 橋能力清單：門戶頁 fetch /ping 看到 "win" 才啟用自繪頂欄（舊版桌面殼沒有→仍用系統標題欄，避免雙標題欄）。
//   "oauth" = 支持本地橋 loopback 授權（反寫 Google 表）；門戶頁見到才走橋授權，否則退回 GIS 彈窗。
const BRIDGE_CAPS: &[&str] = &["file", "win", "badge", "child", "update", "oauth"];

// ───────── 反寫 Google 表的 OAuth（桌面 WebView2 攔 GIS 彈窗 → 走系統瀏覽器 loopback）─────────
//   流程：門戶頁 POST /oauth/start → 橋開系統瀏覽器授權頁 → Google 重定向到
//   http://127.0.0.1:3710/oauth2callback#access_token=... → 回調 HTML 用 JS 讀 fragment
//   回 POST /oauth/store 存進橋 → 門戶頁輪詢 GET /oauth/token 取得 token。
//   用 implicit（response_type=token）避免在 Rust 側做 token 交換（無需額外 HTTP client 依賴）。
//   ⚠️ 前置：Google Cloud 該 OAuth 客戶端要把「http://127.0.0.1:3710/oauth2callback」加入授權重定向 URI。
const OAUTH_CLIENT_ID: &str = "637230075865-lfeagh9nb7j3v5p9a1ppk6mpi252b157.apps.googleusercontent.com";
const OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/spreadsheets";
const OAUTH_REDIRECT: &str = "http://127.0.0.1:3710/oauth2callback";
// 存 (access_token, 到期毫秒時間戳)。Mutex::new 為 const fn，故可直接作 static。
static OAUTH_TOKEN: Mutex<Option<(String, u128)>> = Mutex::new(None);

// 授權完成後回調頁：伺服器讀不到 URL fragment，交由此頁 JS 讀取 #access_token 再回 POST。
const OAUTH_CALLBACK_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>Google 授權</title></head>
<body style="font-family:system-ui,sans-serif;text-align:center;padding:48px 24px;color:#333">
<h2 id="m">正在完成授權…</h2>
<script>(function(){
  var p=new URLSearchParams(location.hash.replace(/^#/,''));
  var tok=p.get('access_token'), exp=p.get('expires_in');
  var m=document.getElementById('m');
  if(!tok){m.textContent='授權失敗：未取得存取權杖，請關閉此頁後重試。';return;}
  fetch('/oauth/store',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({access_token:tok,expires_in:Number(exp)||3600})})
    .then(function(){m.textContent='✓ 授權成功，請返回「華利建機處」應用繼續，可關閉此頁。';})
    .catch(function(){m.textContent='寫回本地失敗，請重試。';});
})();</script></body></html>"#;

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn oauth_auth_url() -> String {
    let scope = urlencoding::encode(OAUTH_SCOPE);
    let redirect = urlencoding::encode(OAUTH_REDIRECT);
    format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=token&scope={}&prompt=consent&include_granted_scopes=true",
        OAUTH_CLIENT_ID, redirect, scope
    )
}

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

// 只允許門戶頁跨域讀取（token 端點限縮來源，避免其它網站 fetch 本機橋竊取剛簽發的 Sheets token）。
fn json_resp_portal(v: Value) -> Resp {
    Response::from_string(v.to_string())
        .with_header(header("Content-Type", "application/json; charset=utf-8"))
        .with_header(header("Access-Control-Allow-Origin", PORTAL_BASE))
        .with_header(header("Access-Control-Allow-Methods", "GET, POST, OPTIONS"))
        .with_header(header("Access-Control-Allow-Headers", "Content-Type"))
}

// 回調頁走 HTML（同源，供系統瀏覽器渲染）。
fn html_resp(s: &str) -> Resp {
    Response::from_string(s.to_string())
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
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

                // ── 反寫 Google 表：loopback 授權 ──
                "/oauth/start" => {
                    { let mut t = OAUTH_TOKEN.lock().unwrap(); *t = None; } // 清舊 token，強制本次重新授權
                    let ok = app.opener().open_url(oauth_auth_url(), None::<&str>).is_ok();
                    json_resp(json!({"ok": ok}))
                }
                // Google 帶著 #access_token=... 重定向到這；fragment 伺服器讀不到，交回調頁 JS 處理。
                "/oauth2callback" => html_resp(OAUTH_CALLBACK_HTML),
                // 回調頁把 token 回 POST 存進橋。
                "/oauth/store" => {
                    let v = body_json(&mut req);
                    let tok = v.get("access_token").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let exp_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600);
                    if tok.is_empty() {
                        json_resp(json!({"ok": false, "msg": "no token"}))
                    } else {
                        let exp_at = now_ms() + (exp_in as u128) * 1000;
                        { let mut t = OAUTH_TOKEN.lock().unwrap(); *t = Some((tok, exp_at)); }
                        json_resp(json!({"ok": true}))
                    }
                }
                // 門戶頁輪詢取 token（限縮來源，剩餘壽命 <60s 視為無效）。
                "/oauth/token" => {
                    let now = now_ms();
                    let got = { OAUTH_TOKEN.lock().unwrap().clone() };
                    match got {
                        Some((tok, exp)) if exp > now + 60_000 => {
                            json_resp_portal(json!({"ok": true, "access_token": tok, "expires_in": ((exp - now) / 1000) as u64}))
                        }
                        _ => json_resp_portal(json!({"ok": false})),
                    }
                }

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
