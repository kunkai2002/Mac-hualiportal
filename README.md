# 華利建機處 — 桌面軟件（Tauri v2 · 方案 A）

這是門戶的**桌面殼**：打包成一個 Windows 軟件，打開後加載線上門戶（含 PM2）。
**方案 A 的爽點：網頁更新後，桌面下次打開自動就是新版，無需重新打包。**

> 工程是獨立的，**不影響也不依賴 `source_v12/` 網頁項目**。網頁版繼續照常用。

---

## 〇、你的機器現狀（已查）
- ✅ Node v24 / npm 11 —— 有
- ✅ WebView2 v149 —— 已裝（桌面渲染引擎，不用再裝）
- ❌ **Rust** —— 沒裝，**必須先裝**（見第一步）
- ❌ **C++ 生成工具** —— Rust 在 Windows 編譯要用到，**也要裝**

---

## 第一步：裝 Rust + C++ 生成工具（只需一次，約 10–20 分鐘）

**1a. 裝「Visual Studio C++ 生成工具」**（Rust 的 Windows 鏈接器）
- 方式一（推薦，命令行）：以**管理員身份**開 PowerShell，跑：
  ```
  winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  ```
- 方式二（手動）：下載 https://visualstudio.microsoft.com/visual-cpp-build-tools/ → 安裝時勾選 **「使用 C++ 的桌面開發」** 工作負載 → 裝。

**1b. 裝 Rust**
- 去 https://rustup.rs 下載 `rustup-init.exe`，雙擊，**一路按 Enter**（用默認 MSVC）。
  （或命令行：`winget install Rustlang.Rustup`）
- **裝完關掉所有 PowerShell 視窗、重新開一個**（讓 PATH 生效）。
- 驗證（兩條都要有版本號）：
  ```
  rustc --version
  cargo --version
  ```

---

## 第二步：裝本項目依賴
```
cd D:\kun-workspace\huali-desktop
npm install
```

## 第三步：生成圖標（打包必須）
1. 準備一張**方形 PNG**（建議 1024×1024，公司 logo 或任意方圖），放到：
   `D:\kun-workspace\huali-desktop\app-icon.png`
2. 跑（自動生成 `src-tauri/icons/` 全套圖標）：
   ```
   npx tauri icon app-icon.png
   ```
   > 沒有 logo？隨便找張方圖先頂著，以後換掉再重跑即可。

## 第四步：先試跑（可選，不打包，驗證能用）
```
npm run dev
```
- **第一次會編譯 Rust 依賴，5–15 分鐘**（以後幾秒）。
- 會彈出一個桌面視窗，加載線上門戶。能登錄、能用 PM2 就成功了。關掉視窗即結束。

## 第五步：打包成安裝包（.exe）
```
npm run build
```
- 第一次較久。完成後安裝包在：
  ```
  D:\kun-workspace\huali-desktop\src-tauri\target\release\bundle\nsis\HualiPortal_0.1.0_x64-setup.exe
  ```
- 雙擊它安裝 → 開始菜單/桌面出現該應用 → 打開就是門戶（視窗標題「華利建機處」）。

## 第六步：發給同事
- 把上面那個 `*-setup.exe` 發出去，對方雙擊安裝即可。
- ⚠️ **未簽名**：對方首次運行，Windows 會藍框「Windows 已保護你的電腦」→ 點「**更多資訊**」→「**仍要執行**」即可。要去掉這個警告需買**代碼簽名證書**（屬於後續 M4，先內部用可忽略）。

---

## 日常更新怎麼做（重點）
- **改 PM2 / 網頁功能** → 照舊 `cd source_v12 → npm run build → npx wrangler deploy` 部署網頁。
  桌面 app **下次打開自動是新版**，**不用重新打包、不用重裝**。✅
- **只有改「桌面殼本身」**（這個項目：視窗、加原生文件功能等）→ 才需重跑第五步 `npm run build` 出新安裝包發出去。

## 🔴🔴 macOS 兩條（2026-09-17，v0.2.3 修，別改回去）

**① 標題列是分平台的。** `tauri.conf.json` 的 `decorations: false` **只給 Windows**：
那邊的頂欄由網頁自己畫（`source_v12/src/components/DesktopTitleBar.tsx`），
而它的判定是 `window.chrome.webview`（**WebView2 專屬**）。
macOS 走 WKWebView，那個物件永遠不存在 ⇒ 原生標題列被關掉、網頁也不畫
⇒ **整個視窗連紅綠燈都沒有**（jojo：「強行安裝會丟失頂欄」）。
⇒ `tauri.macos.conf.json` 把 `decorations` 覆寫成 `true`，
`main.rs` 的 `setup` 裡再 `set_decorations(true)` 保一層（平台設定檔沒被合併時的兜底）。
**要在 mac 上也用自繪頂欄的話**，得先讓網頁那邊認得出 mac 殼（殼的主視窗載入的是**遠端**頁，
拿不到 Tauri 的 IPC，所以只能靠自訂 User-Agent 或網址參數），不要只把 `decorations` 關掉。

**② universal 的 `.app` 必須自己補 ad-hoc 簽章。**
Rust 連結器給單一架構的執行檔蓋的 ad-hoc 簽名，會在 `lipo` 合成 universal 時被洗掉，
而這個專案沒有設定簽章身分 ⇒ 產物是**無簽章**的 ⇒ Apple Silicon 的 macOS 直接拒絕載入，
畫面上是「**已損毀，無法打開**」——看起來像下載壞了，其實跟下載無關（v0.2.2 的 DMG 就是這樣）。
⇒ CI（`.github/workflows/build-mac.yml`）在 `tauri build` 之後、打 DMG 之前
`codesign --force --deep --sign -`，**而且更新包 `.tar.gz` 要在簽名之後重打再重新簽一次**
（bundler 產的那份裡面是簽名前的 .app）。
★ ad-hoc **不等於**公證：第一次打開仍然要在「系統設定 → 隱私權與安全性」按「仍要打開」。
要免掉那一步得有 Apple Developer 帳號（$99/年）＋ notarytool 公證。

## 想改的地方（都在 `src-tauri/tauri.conf.json`）
- 視窗標題：`app.windows[0].title`
- 默認打開頁：`app.windows[0].url`（想一打開就進 PM2，把結尾改成 `…workers.dev/pm2`）
- 視窗大小：`width` / `height`
- 軟件名 / 安裝包名：`productName`（目前 `HualiPortal`，可改；若改中文偶有編碼坑，建議保持英文，視窗標題用中文即可）

## 常見問題
| 現象 | 原因 / 解法 |
|---|---|
| `cargo not found` | Rust 沒裝好，或裝完沒重開終端 |
| 構建報 `link.exe`/MSVC 找不到 | 第一步 1a 的 C++ 生成工具沒裝，或沒勾 VCTools 工作負載 |
| 圖標相關報錯 | 第三步沒做 / `app-icon.png` 路徑不對 |
| 視窗白屏 | 先確認那個網址在瀏覽器能開；WebView2 是否在（你已裝 v149） |
| 首次 dev/build 很久 | 正常，Rust 首次編譯依賴慢，之後很快 |

---

## 後續路線（對應方案文檔 `source_v12/docs/桌面软件化方案-Tauri.md`）
- **M1（現在）**：殼跑起來 + 出 .exe ← 你正在做這步
- **M2**：在 `src-tauri/src/main.rs` 加 `#[tauri::command]` 原生文件讀寫/監視橋
- **M3**：file-search 文件夾瀏覽 + 拖拽放置（桌面原生）
- **M4**：登錄 deep-link 回調、CORS、代碼簽名、自動更新
