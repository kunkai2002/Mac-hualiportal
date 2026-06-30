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
