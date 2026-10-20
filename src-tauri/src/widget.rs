// ═══════════════════════════════════════════════════════════════════════════
//  桌面行事曆小窗（0.2.4，jojo 2026-10-19）
//
//  jojo：「可以設置成放到電腦桌面上的一個窗口……有個固定按鈕，點擊可以鎖定位置，且窗口不可點擊互動，
//        除非解鎖，可以點擊到窗口之下的其他可交互目標，然後可以調整透明度」。
//  拍板：鎖住時整個小窗滑鼠穿透，**只有釘子那一顆還點得到**；系統匣選單也能解鎖（備用）。
//
//  · 小窗載入門戶的 `/cal-widget?bare=1&dsk=widget`（畫面、資料、透明度滑桿全在網頁那邊）；
//    這裡只管「視窗」：無邊框、透明、置頂、不進工作列、記住位置大小、穿透。
//  · 穿透＝`set_ignore_cursor_events(true)`：整個視窗都點不到。那顆釘子怎麼還點得到？
//    網頁把它在視窗裡的位置（CSS px）用 `/widget/hotspot` 交過來，背景執行緒每 60ms 看一次游標，
//    游標在那一塊上就暫時關掉穿透、離開就再打開。
//  · 狀態（開著沒、鎖了沒、位置大小）存 `app_config_dir/calwidget.json`，重開機照樣在原地。
// ═══════════════════════════════════════════════════════════════════════════
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::menu::CheckMenuItem;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry};

pub const LABEL: &str = "calwidget";

/// 第一次打開的大小（邏輯 px）。
const DEFAULT_W: f64 = 320.0;
const DEFAULT_H: f64 = 460.0;
/// 背景執行緒多久看一次游標。60ms：滑到釘子上幾乎感覺不到延遲，又不會一直吃 CPU。
const POLL: Duration = Duration::from_millis(60);

#[derive(Clone, Copy)]
struct Geo { x: i32, y: i32, w: u32, h: u32 }

struct State {
    open: bool,
    locked: bool,
    geo: Option<Geo>,
    /// 鎖住時唯一點得到的那一塊：視窗內的 CSS px（x, y, w, h）。
    hot: Option<(f64, f64, f64, f64)>,
    /// 現在是不是在穿透（避免每 60ms 重設一次）。
    ignoring: bool,
    poller: bool,
    /// 0.2.6：鎖住時桌面跑到最前面（Win+D）⇒ 暫時改成置頂浮上來；這一格記現在是不是浮著。
    raised: bool,
    last_save: Option<Instant>,
    save_pending: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    open: false, locked: false, geo: None, hot: None,
    ignoring: false, poller: false, raised: false, last_save: None, save_pending: false,
});

/// 小窗要載哪個門戶。除錯版可以用 `HUALI_WIDGET_BASE` 指到鏡像站（正式站還沒有這一頁時測試用）。
fn portal_base() -> String {
    if cfg!(debug_assertions) {
        if let Ok(v) = std::env::var("HUALI_WIDGET_BASE") {
            if v.starts_with("https://") || v.starts_with("http://") { return v.trim_end_matches('/').to_string(); }
        }
    }
    crate::PORTAL_BASE.to_string()
}

fn widget_url() -> Option<tauri::Url> {
    let port = crate::serve::bridge_port();
    // 本機橋不在 3710（除錯版）時告訴網頁改連哪個埠（`desktopBridge.ts` 認 `bp`）。
    let bp = if port == 3710 { String::new() } else { format!("&bp={}", port) };
    format!("{}/cal-widget?bare=1&dsk=widget{}", portal_base(), bp).parse().ok()
}

// ───────── 設定檔 ─────────

fn cfg_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("calwidget.json"))
}

fn load(app: &AppHandle) {
    let Some(p) = cfg_path(app) else { return };
    let Ok(text) = fs::read_to_string(p) else { return };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
    let mut s = STATE.lock().unwrap();
    s.open = v.get("open").and_then(|x| x.as_bool()).unwrap_or(false);
    s.locked = v.get("locked").and_then(|x| x.as_bool()).unwrap_or(false);
    let num = |k: &str| v.get(k).and_then(|x| x.as_i64());
    if let (Some(x), Some(y), Some(w), Some(h)) = (num("x"), num("y"), num("w"), num("h")) {
        if w > 0 && h > 0 { s.geo = Some(Geo { x: x as i32, y: y as i32, w: w as u32, h: h as u32 }); }
    }
}

fn save_now(app: &AppHandle) {
    let Some(p) = cfg_path(app) else { return };
    let body = {
        let mut s = STATE.lock().unwrap();
        s.last_save = Some(Instant::now());
        s.save_pending = false;
        let mut v = json!({ "open": s.open, "locked": s.locked });
        if let Some(g) = s.geo { v["x"] = json!(g.x); v["y"] = json!(g.y); v["w"] = json!(g.w); v["h"] = json!(g.h); }
        v.to_string()
    };
    if let Some(dir) = p.parent() { let _ = fs::create_dir_all(dir); }
    let _ = fs::write(p, body);
}

/// 拖動時一秒會來幾十次 Moved：最多 0.4 秒寫一次檔，最後一次一定會寫到（延後補寫）。
fn save_soon(app: &AppHandle) {
    let wait = {
        let mut s = STATE.lock().unwrap();
        let recent = s.last_save.map(|t| t.elapsed() < Duration::from_millis(400)).unwrap_or(false);
        if !recent { None } else if s.save_pending { return } else { s.save_pending = true; Some(Duration::from_millis(450)) }
    };
    match wait {
        None => save_now(app),
        Some(d) => {
            let app2 = app.clone();
            std::thread::spawn(move || { std::thread::sleep(d); save_now(&app2); });
        }
    }
}

// ───────── 視窗 ─────────

/// 存下來的位置還在某一個螢幕上嗎（換了螢幕、拔了副螢幕之後不要開在看不到的地方）。
fn on_some_monitor(w: &WebviewWindow, g: Geo) -> bool {
    let Ok(mons) = w.available_monitors() else { return false };
    let (cx, cy) = (g.x + 40, g.y + 20);
    mons.iter().any(|m| {
        let (p, s) = (m.position(), m.size());
        cx >= p.x && cy >= p.y && cx < p.x + s.width as i32 && cy < p.y + s.height as i32
    })
}

/// 放到上次的位置；沒有（或那個螢幕不在了）就放在主螢幕右上角。
fn place(w: &WebviewWindow) {
    let geo = STATE.lock().unwrap().geo;
    if let Some(g) = geo {
        if on_some_monitor(w, g) {
            let _ = w.set_size(PhysicalSize::new(g.w, g.h));
            let _ = w.set_position(PhysicalPosition::new(g.x, g.y));
            return;
        }
    }
    if let Ok(Some(m)) = w.primary_monitor() {
        let sf = m.scale_factor();
        let (p, s) = (m.position(), m.size());
        let ww = (DEFAULT_W * sf) as i32;
        let x = p.x + s.width as i32 - ww - (28.0 * sf) as i32;
        let y = p.y + (96.0 * sf) as i32;
        let _ = w.set_position(PhysicalPosition::new(x, y));
    }
}

fn build(app: &AppHandle) {
    let Some(url) = widget_url() else { return };
    #[allow(unused_mut)]
    let mut b = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(url))
        .title("行事曆")
        .inner_size(DEFAULT_W, DEFAULT_H)
        .min_inner_size(240.0, 180.0)
        .decorations(false)
        // 0.2.5（jojo 10-19：「層級太高了，我要的是只有桌面能看到，其他能在這個窗口上面的」）：
        // 貼在桌面那一層 —— 其他視窗都蓋得過它，點它也不會浮上來。不能縮小（「顯示桌面」會想把它縮掉，見 on_event）。
        .always_on_bottom(true)
        .minimizable(false)
        .skip_taskbar(true)
        .shadow(false)
        .resizable(true)
        // 不搶焦點：開機自動恢復時，使用者可能正在別的地方打字（誤判 415：被搶走的 Enter 會按到別的鍵）
        .focused(false)
        .visible(false);
    // 透明視窗＝圓角與半透明底都由網頁畫。macOS 要開私有 API 才有這個選項，那邊先用不透明的。
    #[cfg(not(target_os = "macos"))]
    {
        b = b.transparent(true);
    }
    if let Ok(w) = b.build() {
        place(&w);
        let _ = w.show();
        let locked = STATE.lock().unwrap().locked;
        if locked { apply_ignore(app, true, true); start_poller(app.clone()); }
    }
}

/// 系統匣「桌面行事曆」那一格（打勾＝開著）。小窗從別處開關（網頁的關閉鍵、本機橋、啟動恢復）也要跟著改勾。
static TRAY_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();

pub fn bind_tray_item(item: CheckMenuItem<Wry>) {
    let _ = TRAY_ITEM.set(item);
}

fn sync_tray(open: bool) {
    if let Some(item) = TRAY_ITEM.get() { let _ = item.set_checked(open); }
}

/// 系統匣按「桌面行事曆」：開著就關、關著就開。
/// 🔴 這是一定關得掉的出口：網頁沒載進來（斷網、門戶連不上、舊版畫面）時，小窗裡沒有關閉鍵，
///    它又置頂、無邊框，只剩這裡能關（不然只能退出整個程式，下次開機還照「上次開著」再打開）。
pub fn toggle(app: &AppHandle) {
    let open_now = STATE.lock().unwrap().open && app.get_webview_window(LABEL).is_some();
    if open_now { close(app); } else { open(app); }
}

/// 打開（已經開著就叫到前面來）。
pub fn open(app: &AppHandle) {
    STATE.lock().unwrap().open = true;
    save_now(app);
    sync_tray(true);
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = app2.get_webview_window(LABEL) {
            let _ = w.show();
            let _ = w.unminimize();
        } else {
            build(&app2);
        }
    });
}

pub fn close(app: &AppHandle) {
    {
        let mut s = STATE.lock().unwrap();
        s.open = false;
        s.locked = false;      // 關掉就不留鎖：下次打開先能動、能調
        s.ignoring = false;
        s.raised = false;      // 下次打開是新建的視窗，本來就在最底層
    }
    save_now(app);
    sync_tray(false);
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = app2.get_webview_window(LABEL) { let _ = w.close(); }
    });
}

pub fn state() -> Value {
    let s = STATE.lock().unwrap();
    json!({ "ok": true, "open": s.open, "locked": s.locked })
}

/// 啟動時：上次開著就照原樣打開（含鎖定）。
pub fn restore(app: &AppHandle) {
    load(app);
    if STATE.lock().unwrap().open { open(app); }
}

// ───────── 鎖定＝穿透 ─────────

fn apply_ignore(app: &AppHandle, ignore: bool, force: bool) {
    {
        let mut s = STATE.lock().unwrap();
        if !force && s.ignoring == ignore { return; }
        s.ignoring = ignore;
    }
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = app2.get_webview_window(LABEL) { let _ = w.set_ignore_cursor_events(ignore); }
    });
}

pub fn set_locked(app: &AppHandle, on: bool) {
    STATE.lock().unwrap().locked = on;
    save_now(app);
    apply_ignore(app, on, true);
    if on { start_poller(app.clone()); }
    // 告訴小窗網頁現在鎖了沒 —— 系統匣「解鎖」不是網頁按的，不講它會一直畫成鎖住的樣子。
    // 網頁還沒載好就收不到，沒關係：網頁一掛上去會自己問一次 /widget/state。
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = app2.get_webview_window(LABEL) {
            let _ = w.eval(&format!("window.dispatchEvent(new CustomEvent('huali:widget-lock',{{detail:{on}}}))"));
        }
    });
}

pub fn set_hotspot(x: f64, y: f64, w: f64, h: f64) {
    if w > 0.0 && h > 0.0 { STATE.lock().unwrap().hot = Some((x, y, w, h)); }
}

/// 游標（螢幕實體 px）在不在熱區裡。熱區是視窗內的 CSS px ⇒ 先扣掉視窗左上角、再除以縮放倍率。
pub fn hot_contains(hot: (f64, f64, f64, f64), cursor: (f64, f64), win_origin: (f64, f64), scale: f64) -> bool {
    if scale <= 0.0 { return false; }
    let (x, y, w, h) = hot;
    let lx = (cursor.0 - win_origin.0) / scale;
    let ly = (cursor.1 - win_origin.1) / scale;
    lx >= x && lx <= x + w && ly >= y && ly <= y + h
}

// ───────── 鎖住時 Win+D 也不消失（0.2.6） ─────────
// jojo 10-20：「我還想當我鎖定後哪怕 win+D 都不會消失」。
// 「顯示桌面」不是把視窗縮小，是把**桌面本身**拉到所有視窗前面 —— 貼在最底層的小窗因此被蓋住。
// ⇒ 鎖住時順便看前景是不是桌面；是就暫時改成置頂浮上來，切回任何別的視窗就沉回最底層。

/// 前景是不是桌面本身：Win+D 之後是 `WorkerW`、點了桌面空白處是 `Progman`（Windows 10／11）。
#[cfg(windows)]
fn desktop_is_foreground() -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        fn GetClassNameW(hwnd: isize, name: *mut u16, max: i32) -> i32;
    }
    if cfg!(debug_assertions) {
        // 測試用：除錯版指一個檔，檔在就當成桌面在最前面（不用真的按 Win+D 打擾正在用電腦的人）
        if let Ok(p) = std::env::var("HUALI_WIDGET_FAKE_DESKTOP") {
            if !p.is_empty() { return std::path::Path::new(&p).exists(); }
        }
    }
    let h = unsafe { GetForegroundWindow() };
    if h == 0 { return false; }
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 { return false; }
    let name = String::from_utf16_lossy(&buf[..n as usize]);
    name == "WorkerW" || name == "Progman"
}
#[cfg(not(windows))]
fn desktop_is_foreground() -> bool { false }

/// 浮上來（置頂）／沉回最底層。狀態沒變就不動。
fn set_raised(app: &AppHandle, up: bool) {
    {
        let mut s = STATE.lock().unwrap();
        if s.raised == up { return; }
        s.raised = up;
    }
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(w) = app2.get_webview_window(LABEL) {
            if up { let _ = w.set_always_on_bottom(false); let _ = w.set_always_on_top(true); }
            else { let _ = w.set_always_on_top(false); let _ = w.set_always_on_bottom(true); }
        }
    });
}

fn start_poller(app: AppHandle) {
    {
        let mut s = STATE.lock().unwrap();
        if s.poller { return; }
        s.poller = true;
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(POLL);
            let (locked, hot, raised) = { let s = STATE.lock().unwrap(); (s.locked, s.hot, s.raised) };
            let Some(w) = app.get_webview_window(LABEL) else { break };
            // 鎖住：桌面在最前面就浮上來。解鎖時還浮著：等前景離開桌面再沉下去（不在使用者眼前突然消失）。
            let desk_fg = desktop_is_foreground();
            if locked { set_raised(&app, desk_fg); }
            else if raised && !desk_fg { set_raised(&app, false); }
            if !locked {
                if STATE.lock().unwrap().raised { continue; }
                break;
            }
            let over = match (hot, w.cursor_position(), w.inner_position(), w.scale_factor()) {
                (Some(r), Ok(c), Ok(p), Ok(sf)) => hot_contains(r, (c.x, c.y), (p.x as f64, p.y as f64), sf),
                _ => false,
            };
            apply_ignore(&app, !over, false);
        }
        {
            let mut s = STATE.lock().unwrap();
            s.poller = false;
            if app.get_webview_window(LABEL).is_none() { s.raised = false; }   // 視窗沒了：下次開是新建的（最底層）
        }
        // 解鎖了（或視窗沒了）：一定回到「點得到」
        if !STATE.lock().unwrap().locked { apply_ignore(&app, false, true); }
    });
}

// ───────── 視窗事件（main.rs 轉過來） ─────────

/// 移動／改大小：記下來（節流寫檔）。關掉：記成沒開。
pub fn on_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    match event {
        tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
            // 被「顯示桌面」（Win+D）之類縮掉：馬上叫回來 —— 它本來就在最底層，回來也只在桌面上看得到。
            // 縮小時位置是 (-32000,-32000)，不能記進設定檔。
            if window.is_minimized().unwrap_or(false) {
                let _ = window.unminimize();
                return;
            }
            if let (Ok(p), Ok(sz)) = (window.outer_position(), window.inner_size()) {
                if sz.width > 0 && sz.height > 0 {
                    STATE.lock().unwrap().geo = Some(Geo { x: p.x, y: p.y, w: sz.width, h: sz.height });
                    save_soon(window.app_handle());
                }
            }
        }
        tauri::WindowEvent::Destroyed => {
            {
                let mut s = STATE.lock().unwrap();
                s.open = false;
                s.ignoring = false;
                s.raised = false;
            }
            save_now(window.app_handle());
            sync_tray(false);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::hot_contains;

    #[test]
    fn hotspot_math() {
        // 視窗左上角在 (1000, 200)、縮放 1.5；熱區是視窗內 CSS (280, 4, 32, 32)
        let hot = (280.0, 4.0, 32.0, 32.0);
        // 游標在熱區中心：CSS (296, 20) ⇒ 實體 (1000+444, 200+30)
        assert!(hot_contains(hot, (1444.0, 230.0), (1000.0, 200.0), 1.5));
        // 往左 20 個 CSS px 就出去了
        assert!(!hot_contains(hot, (1414.0, 230.0), (1000.0, 200.0), 1.5));
        // 縮放 1：直接相減
        assert!(hot_contains(hot, (1290.0, 210.0), (1000.0, 200.0), 1.0));
        assert!(!hot_contains(hot, (1290.0, 210.0), (1000.0, 200.0), 0.0));
    }
}
