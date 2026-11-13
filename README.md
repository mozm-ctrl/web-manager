## Web Manager · 网页管理器

一个桌面端的多网站、多账号管理器。左侧管理网站和账号，右侧用**每个账号独立的浏览器环境**打开网站：同一个网站可以同时登录好几个号，Cookie 和登录状态互不影响，切换账号不用退出重登，关掉 app 再打开也还是登录状态。

![](docs/2-account-b-bob.png)

## 下载

到 [Releases](https://github.com/mozm-ctrl/web-manager/releases/latest) 页面下载：

| 电脑 | 下载哪个 | 系统要求 |
| --- | --- | --- |
| Mac（Apple 芯片和 Intel 芯片都可以） | `Web.Manager_0.3.0_universal.dmg` | macOS 14 及以上 |
| Windows | `Web.Manager_0.3.0_windows_x64.zip` | Windows 10 / 11，64 位 |

## 安装

### Mac
1. 双击 `.dmg`，在弹出的窗口里把 **Web Manager** 拖进「应用程序」。
2. 在访达侧栏点安装盘旁边的 ⏏ 推出安装盘（不推出的话启动台里会出现两个图标）。
3. 第一次打开时，系统会提示「无法验证开发者」或「已损坏」。这是因为安装包没有花钱做 Apple 签名，app 本身没有问题。放行方法二选一：
   - 「系统设置 → 隐私与安全性」拉到底部，点 **仍要打开**；
   - 或者在终端执行：`xattr -dr com.apple.quarantine "/Applications/Web Manager.app"`
4. 第一次保存或查看账号密码时，系统可能会问是否允许访问钥匙串，点「始终允许」。

### Windows
1. 解压 zip，把 `Web Manager.exe` 放到一个固定的文件夹，双击运行，不需要安装。
2. 如果弹出「Windows 已保护你的电脑」，点「更多信息 → 仍要运行」（同样是因为没有购买代码签名证书）。

> ⚠️ Windows 版是在 Mac 上交叉编译的，还没有在真机上完整测试过。遇到问题欢迎提 Issue。

## 使用说明

1. **添加网站**：点左上角 ＋，填名称和网址（比如 `GitHub`、`https://github.com`）。
2. **添加账号**：鼠标移到网站那一行，点右侧的 ＋，给账号起个备注名（比如「工作号」「生活号」）。一个网站可以加任意多个账号。
3. **登录**：点左侧某个账号，右边就会打开网站，在里面正常登录一次。之后点哪个账号就是哪个号，登录状态一直保留。
4. **多标签页**：打开账号后，网页上方有标签栏，＋ 新建标签，× 关闭。同一账号的标签页共享登录状态；网页里「在新窗口打开」的链接会变成这个账号的新标签页。
5. **浏览**：顶部有后退、前进、刷新、回首页按钮和地址栏，可以直接输入网址跳转。
6. **记住账号密码**：鼠标移到账号上点 🔑（或打开网页后点工具栏上的 🔑），填写账号、密码、备注后保存。之后可以随时查看，点「显示」看密码，点「复制」粘贴到登录框。
7. **改名 / 删除**：鼠标移到网站或账号上，点 ✎ 修改，点 ✕ 删除。删除账号会同时清除它的登录数据和保存的密码。

![](docs/5-tabs.png)

![](docs/4-credential.png)

## 主要用到的技术

| 部分 | 用了什么 | 作用 |
| --- | --- | --- |
| 应用框架 | [Tauri 2](https://tauri.app/)（Rust） | 创建窗口、把网页嵌进窗口、调用系统功能 |
| 界面 | React + TypeScript + Vite | 左侧网站/账号列表、标签栏、对话框 |
| 网页引擎（Mac） | 系统自带的 WebKit（Safari 的引擎） | 显示网页、保存 Cookie |
| 网页引擎（Windows） | 系统自带的 WebView2（Edge / Chromium 内核） | 显示网页、保存 Cookie |
| 账号隔离 | Mac：`WKWebsiteDataStore` 独立数据仓库（macOS 14+）；Windows：每个账号单独的 WebView2 数据目录 | 每个账号一份 Cookie / LocalStorage，互不干扰 |
| 密码保存 | [keyring](https://crates.io/crates/keyring)：Mac 钥匙串 / Windows 凭据管理器 | 由系统加密保存账号密码 |

因为网页引擎用的是系统自带的，安装包只有几 MB。

### 账号隔离原理

网站靠浏览器里存的 Cookie 认出「你是谁」。普通浏览器里一个网站只有一份 Cookie，所以登录第二个号会把第一个顶掉。这个 app 给每个账号分配一个固定编号（UUID），再用这个编号向系统网页引擎申请一个独立的数据仓库。每个账号的网页只能读写自己的仓库，所以对网站来说，就像是几台不同电脑上的浏览器。

### 数据存在哪里

所有数据只在本机，不会上传：

| 内容 | Mac | Windows |
| --- | --- | --- |
| 网站和账号列表（不含密码） | `~/Library/Application Support/com.mozm.webmanager/sites.json` | `%APPDATA%\com.mozm.webmanager\sites.json` |
| 每个账号的 Cookie 和网页缓存 | `~/Library/WebKit/com.mozm.webmanager/` | `%APPDATA%\com.mozm.webmanager\profiles\` |
| 保存的账号密码 | 钥匙串（搜索 `com.mozm.webmanager`） | 凭据管理器（搜索 `com.mozm.webmanager`） |

直接删除 app 不会删除这些数据；想彻底清理，先在 app 里删掉所有网站，再删除上面这些位置。

## 已知限制

- 只隔离登录状态，**不隔离 IP 和设备特征**。风控严格的平台（电商、社交媒体等）仍可能识别出多个账号属于同一个人。
- Mac 需要 macOS 14 及以上，更低版本无法隔离账号。
- 依赖弹窗回传结果的第三方登录（部分「用 Google / 微信登录」）可能无法完成，可以改用账号密码登录。
- 标签页不会在重启后恢复（登录状态会保留）；打开的标签页都常驻内存，开很多会变卡。
- 账号密码只能查看和复制，不会自动填写；查看前不需要额外验证。
- 打开对话框时右侧网页会暂时隐藏。
- 安装包未签名，首次打开需要手动放行。

## 从源码构建

需要 [Rust](https://rustup.rs/)、Node.js 20+、pnpm 9+，以及 [Tauri 的系统依赖](https://tauri.app/start/prerequisites/)（Mac 只需 Xcode Command Line Tools）。

```bash
pnpm install
pnpm tauri dev      # 开发模式运行
pnpm tauri build    # 打包当前平台的安装包
```

Mac 通用包（Intel + Apple 芯片）：

```bash
rustup target add x86_64-apple-darwin aarch64-apple-darwin
pnpm tauri build --target universal-apple-darwin
```

在 Mac 上交叉编译 Windows 版：需要 `cargo install cargo-xwin`，以及 PATH 里有 `clang-cl`、`lld-link`、`llvm-lib`、`llvm-rc`。

```bash
rustup target add x86_64-pc-windows-msvc
pnpm tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --no-bundle
```

`ci/release.yml` 是 GitHub Actions 工作流，可以在真实的 Mac / Windows 机器上打包并挂到 Release。启用方法：把它复制到 `.github/workflows/release.yml`，然后在 Actions 页面手动运行或推送 `v*` 标签。

### 代码结构

| 文件 | 内容 |
| --- | --- |
| `src-tauri/src/lib.rs` | 后端：账号 WebView、标签页、账号隔离、删除清理 |
| `src-tauri/src/store.rs` | 网站和账号列表的读写（`sites.json`） |
| `src-tauri/src/credentials.rs` | 账号密码读写系统钥匙串 |
| `src/App.tsx`、`src/App.css` | 界面 |
| `src/api.ts` | 界面调用后端的接口 |
