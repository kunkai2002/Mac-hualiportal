// 華利建機處 桌面殼（Tauri v2）
// 方案 A：主視窗加載線上門戶 URL（見 tauri.conf.json 的 app.windows[].url）。
// 桌面原生：系統托盤常駐 + 開機自啟 + 關窗縮托盤後台運行。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod serve;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

const PORTAL_BASE: &str = "https://huali-structure-app.qiaoyuhua2002.workers.dev";

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

// 托盤點某個預設應用 → 主視窗導航到該路由並顯示
fn open_app(app: &tauri::AppHandle, route: &str) {
    if let Some(w) = app.get_webview_window("main") {
        let url = format!("{}{}", PORTAL_BASE, route);
        let _ = w.eval(&format!("window.location.assign('{}')", url.replace('\'', "%27")));
    }
    show_main(app);
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .setup(|app| {
            // 啟動內嵌本地橋（127.0.0.1:3710）：本地文件能力 + 原生窗口控制 + 更新；遠端門戶頁 fetch 即可（拿不到 IPC）。
            serve::start_bridge(app.handle().clone());

            // 預設開啟「開機自啟」（用戶可在系統設定 / 托盤後續關閉）
            let _ = app.autolaunch().enable();

            // 系統托盤：左鍵打開、右鍵菜單（打開 / 退出）
            let show = MenuItem::with_id(app, "show", "打開 華利建機處", true, None::<&str>)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let m_home = MenuItem::with_id(app, "app:/", "應用匯總", true, None::<&str>)?;
            let m_pm = MenuItem::with_id(app, "app:/pm2", "PM 項目進度", true, None::<&str>)?;
            let m_fs = MenuItem::with_id(app, "app:/file-search-v2", "文件查找", true, None::<&str>)?;
            let m_cal = MenuItem::with_id(app, "app:/calendar", "行事曆", true, None::<&str>)?;
            let m_sheets = MenuItem::with_id(app, "app:/sheets", "文件中轉站", true, None::<&str>)?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            // 在新視窗開啟（子窗口，獨立於主窗）
            let nw = Submenu::with_items(app, "在新視窗開啟", true, &[
                &MenuItem::with_id(app, "nw:/pm2", "PM 項目進度", true, None::<&str>)?,
                &MenuItem::with_id(app, "nw:/file-search-v2", "文件查找", true, None::<&str>)?,
                &MenuItem::with_id(app, "nw:/calendar", "行事曆", true, None::<&str>)?,
                &MenuItem::with_id(app, "nw:/sheets", "文件中轉站", true, None::<&str>)?,
            ])?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &sep1, &m_home, &m_pm, &m_fs, &m_cal, &m_sheets, &nw, &sep2, &quit])?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("華利建機處")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    let id = event.id.as_ref();
                    if id == "show" {
                        show_main(app);
                    } else if id == "quit" {
                        app.exit(0);
                    } else if let Some(route) = id.strip_prefix("nw:") {
                        serve::open_app_window(app, route);
                    } else if let Some(route) = id.strip_prefix("app:") {
                        open_app(app, route);
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // 若由開機自啟帶 --minimized 啟動 → 直接縮到後台（托盤），不彈窗
            if std::env::args().any(|a| a == "--minimized") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }
            Ok(())
        })
        // 關閉窗口：主窗縮托盤後台（順帶隱藏所有子窗 → 主窗關，子窗全關）；子窗放行真正關閉。
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let app = window.app_handle();
                    for (_, w) in app.webview_windows() { let _ = w.hide(); }
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
