mod credentials;
mod store;

use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::webview::{DownloadEvent, NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Rect, State, Url, Webview,
    WebviewBuilder, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_opener::OpenerExt;
use uuid::Uuid;

use store::{Account, Site, Store};

/// Content area (logical px) reported by the frontend; account webviews fill it.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    /// `window.innerHeight` of the sidebar UI, used to detect a title bar that
    /// overlaps the window's content view (macOS).
    viewport_height: f64,
}

struct AppState {
    store: Mutex<Store>,
    store_path: PathBuf,
    /// Per-account browser data directories (Windows / Linux only).
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    profiles_dir: PathBuf,
    /// Account currently shown on the right.
    active: Mutex<Option<Uuid>>,
    /// Open tabs per account (runtime only). All tabs of an account share its data store.
    tabs: Mutex<HashMap<Uuid, AccountTabs>>,
    bounds: Mutex<Bounds>,
    /// True while a dialog is open: the sidebar UI is raised above the account
    /// webviews instead of below them (see `set_overlay`).
    overlay: Mutex<bool>,
    /// Every saved login, once the Keychain vault was unlocked this session (at most
    /// one password prompt per launch). Memory only; never written anywhere else.
    vault: Mutex<Option<credentials::Vault>>,
    /// Downloads started but not yet finished, by URL (see `handle_download`).
    downloads: Mutex<DownloadQueue>,
}

#[derive(Default)]
struct AccountTabs {
    tabs: Vec<Uuid>,
    active: Option<Uuid>,
}

impl AppState {
    fn account_of_tab(&self, tab_id: Uuid) -> Option<Uuid> {
        self.tabs
            .lock()
            .unwrap()
            .iter()
            .find(|(_, t)| t.tabs.contains(&tab_id))
            .map(|(acc, _)| *acc)
    }

    fn save(&self, store: &Store) -> Result<(), String> {
        store.save(&self.store_path)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    account_id: String,
    tab_id: String,
    url: String,
    title: Option<String>,
    /// `None` when the event says nothing about loading (e.g. a title change).
    loading: Option<bool>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TabsInfo {
    account_id: String,
    tabs: Vec<String>,
    active_tab: Option<String>,
}

fn webview_label(tab_id: &Uuid) -> String {
    format!("tab-{}", tab_id.simple())
}

fn tab_webview(app: &AppHandle, tab_id: &Uuid) -> Option<Webview> {
    app.get_webview(&webview_label(tab_id))
}

fn emit_tabs(app: &AppHandle, account_id: Uuid) {
    let state = app.state::<AppState>();
    let info = {
        let tabs = state.tabs.lock().unwrap();
        let t = tabs.get(&account_id);
        TabsInfo {
            account_id: account_id.to_string(),
            tabs: t.map(|t| t.tabs.iter().map(Uuid::to_string).collect()).unwrap_or_default(),
            active_tab: t.and_then(|t| t.active).map(|id| id.to_string()),
        }
    };
    let _ = app.emit_to("main", "account-tabs", info);
}

/// Closes every tab webview of an account.
fn close_account_tabs(app: &AppHandle, account_id: Uuid) {
    let state = app.state::<AppState>();
    let removed = state.tabs.lock().unwrap().remove(&account_id);
    for tab in removed.map(|t| t.tabs).unwrap_or_default() {
        if let Some(wv) = tab_webview(app, &tab) {
            let _ = wv.close();
        }
    }
}

fn rect(b: Bounds) -> Rect {
    Rect {
        position: LogicalPosition::new(b.x, b.y).into(),
        size: LogicalSize::new(b.width.max(1.0), b.height.max(1.0)).into(),
    }
}

fn parse_url(raw: &str) -> Result<Url, String> {
    let raw = raw.trim();
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };
    let url = Url::parse(&with_scheme).map_err(|e| format!("网址无效：{e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        _ => Err("只支持 http / https 网址".into()),
    }
}

/// Removes everything the browser engine stored for an account.
async fn purge_account_data(app: &AppHandle, account_id: Uuid) -> Result<(), String> {
    close_account_tabs(app, account_id);

    #[cfg(target_os = "macos")]
    {
        // WebKit refuses to remove a data store that is still referenced by a live
        // webview, and closing is asynchronous, so retry for a short while.
        let mut last_err = String::new();
        for _ in 0..20 {
            match app.remove_data_store(*account_id.as_bytes()).await {
                Ok(()) => return Ok(()),
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        Err(last_err)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let state = app.state::<AppState>();
        let dir = state.profiles_dir.join(account_id.to_string());
        let mut last_err = String::new();
        for _ in 0..20 {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        Err(last_err)
    }
}

/// Deletes account data, remembering failures so they are retried on next start.
async fn delete_accounts_data(app: &AppHandle, ids: Vec<Uuid>) {
    let state = app.state::<AppState>();
    for id in ids {
        // A locked vault is cleaned up when it is next unlocked (see `prune_vault`).
        let unlocked = {
            let mut slot = state.vault.lock().unwrap();
            slot.as_mut().and_then(|v| v.remove(&id).map(|_| v.clone()))
        };
        if let Some(vault) = unlocked {
            if let Err(e) = credentials::save_vault(&vault) {
                eprintln!("failed to remove saved login of account {id}: {e}");
            }
        }
        if let Some(w) = app.get_webview_window(&peek_label(&id)) {
            let _ = w.close();
        }
        if credentials::exists(id).unwrap_or(false) {
            if let Err(e) = credentials::delete(id) {
                eprintln!("failed to delete saved login for account {id}: {e}");
            }
        }
        {
            let mut active = state.active.lock().unwrap();
            if *active == Some(id) {
                *active = None;
            }
        }
        if let Err(e) = purge_account_data(app, id).await {
            eprintln!("failed to purge data for account {id}: {e}");
            let mut store = state.store.lock().unwrap();
            if !store.pending_purge.contains(&id) {
                store.pending_purge.push(id);
            }
            let _ = state.save(&store);
        }
    }
}

fn emit_page(app: &AppHandle, account_id: Uuid, tab_id: Uuid, url: &Url, title: Option<String>, loading: Option<bool>) {
    let _ = app.emit_to(
        "main",
        "account-page",
        PageInfo {
            account_id: account_id.to_string(),
            tab_id: tab_id.to_string(),
            url: url.to_string(),
            title,
            loading,
        },
    );
}

/// WKWebView's default user agent has no `Version/… Safari/…` part. Some sites
/// (notably Chinese ones such as Baidu) treat that as an unknown browser and serve
/// it differently, e.g. redirecting in a loop, so present as Safari.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";

/// Pages ask for a new tab by navigating to this URL (see `NEW_TAB_SCRIPT`).
const NEW_TAB_PREFIX: &str = "https://new-tab.web-manager.invalid/?u=";

/// WKWebView does not reliably report `target=_blank` clicks for child webviews,
/// so route them (and `window.open`) through a navigation we can intercept.
const NEW_TAB_SCRIPT: &str = r#"(() => {
  const PREFIX = "https://new-tab.web-manager.invalid/?u=";
  const openTab = (u) => {
    try { u = new URL(u, location.href).href; } catch (_) { return; }
    location.href = PREFIX + encodeURIComponent(u);
  };
  document.addEventListener("click", (e) => {
    if (e.defaultPrevented || e.button !== 0) return;
    const a = e.target instanceof Element ? e.target.closest("a[href]") : null;
    if (!a) return;
    const t = (a.getAttribute("target") || "").toLowerCase();
    if (t === "_blank" || e.metaKey) {
      e.preventDefault();
      openTab(a.href);
    }
  }, true);
  window.open = (u) => { if (u) openTab(String(u)); return null; };
})();"#;

fn site_url_of(app: &AppHandle, account_id: Uuid) -> Result<Url, String> {
    let state = app.state::<AppState>();
    let store = state.store.lock().unwrap();
    let site = store
        .sites
        .iter()
        .find(|s| s.accounts.iter().any(|a| a.id == account_id))
        .ok_or("账号不存在")?;
    let account_url = site.accounts.iter().find(|a| a.id == account_id).and_then(|a| a.url.as_deref());
    parse_url(account_url.unwrap_or(&site.url))
}

/// Opens a new tab for the account (at `url`, or the site's home page) and makes it active.
fn open_tab(app: &AppHandle, account_id: Uuid, url: Option<Url>) -> Result<Uuid, String> {
    let state = app.state::<AppState>();
    let url = match url {
        Some(u) => u,
        None => site_url_of(app, account_id)?,
    };
    let tab_id = Uuid::new_v4();
    let label = webview_label(&tab_id);

    let app_for_popup = app.clone();
    let app_for_nav = app.clone();
    let app_for_load = app.clone();
    let app_for_title = app.clone();
    let app_for_download = app.clone();

    #[allow(unused_mut)]
    let mut builder = WebviewBuilder::new(&label, WebviewUrl::External(url.clone()))
        .user_agent(USER_AGENT)
        .initialization_script(NEW_TAB_SCRIPT)
        .on_navigation(move |url| {
            let Some(rest) = url.as_str().strip_prefix(NEW_TAB_PREFIX) else {
                return true;
            };
            let target = Url::parse(rest)
                .ok()
                .or_else(|| url.query_pairs().find(|(k, _)| k == "u").and_then(|(_, v)| Url::parse(&v).ok()));
            if let Some(target) = target.filter(|u| matches!(u.scheme(), "http" | "https")) {
                let app = app_for_nav.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    if let Err(e) = open_tab(&app, account_id, Some(target)) {
                        eprintln!("failed to open new tab: {e}");
                    }
                });
            }
            false
        })
        .on_page_load(move |_wv, payload| {
            let loading = matches!(payload.event(), PageLoadEvent::Started);
            emit_page(&app_for_load, account_id, tab_id, payload.url(), None, Some(loading));
        })
        .on_document_title_changed(move |wv, title| {
            if let Ok(url) = wv.url() {
                emit_page(&app_for_title, account_id, tab_id, &url, Some(title), None);
            }
        })
        // target=_blank / window.open become a new tab of the same account, so they
        // keep this account's cookies. Building a webview blocks on the main thread,
        // which is where this handler runs, so do it from a worker.
        .on_new_window(move |url, _features| {
            let app = app_for_popup.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = open_tab(&app, account_id, Some(url)) {
                    eprintln!("failed to open new tab: {e}");
                }
            });
            NewWindowResponse::Deny
        })
        // Without a handler wry does not install a download delegate on macOS, and
        // downloads do nothing at all.
        .on_download(move |_wv, event| handle_download(&app_for_download, account_id, event));

    #[cfg(target_os = "macos")]
    {
        builder = builder.data_store_identifier(*account_id.as_bytes());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = state.profiles_dir.join(account_id.to_string());
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        builder = builder.data_directory(dir);
    }

    let window = app.get_window("main").ok_or("main window missing")?;
    let b = *state.bounds.lock().unwrap();
    window
        .add_child(
            builder,
            LogicalPosition::new(b.x, b.y),
            LogicalSize::new(b.width.max(1.0), b.height.max(1.0)),
        )
        .map_err(|e| e.to_string())?;

    {
        let mut tabs = state.tabs.lock().unwrap();
        let t = tabs.entry(account_id).or_default();
        t.tabs.push(tab_id);
        t.active = Some(tab_id);
    }
    // The engine reports nothing until the site first responds, which can take a
    // second or more; until then the tab would read 「新标签页」 over a blank page. Show
    // where it is going, loading, from the start.
    emit_page(app, account_id, tab_id, &url, None, Some(true));
    emit_tabs(app, account_id);
    apply_visibility(app);
    // A new child view is added on top; keep the UI above it while a dialog is open.
    if *state.overlay.lock().unwrap() {
        raise_ui(app, true);
    }
    Ok(tab_id)
}

/// Downloads started but not yet finished, oldest first per URL: the same URL can be
/// downloaded several times in a row, and `Finished` only names the URL.
type DownloadQueue = HashMap<String, VecDeque<PendingDownload>>;

#[derive(Debug, Clone, PartialEq)]
struct PendingDownload {
    id: Uuid,
    account_id: Uuid,
    path: PathBuf,
}

impl PendingDownload {
    fn info(&self, url: &str, state: &str) -> DownloadInfo {
        DownloadInfo {
            id: self.id.to_string(),
            account_id: self.account_id.to_string(),
            url: url.to_string(),
            file_name: self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            path: self.path.to_string_lossy().into_owned(),
            state: state.to_string(),
        }
    }
}

/// Payload of the `download` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadInfo {
    id: String,
    account_id: String,
    url: String,
    file_name: String,
    path: String,
    /// "started" | "done" | "failed"
    state: String,
}

impl DownloadInfo {
    /// A download whose request was never seen; reported with what the URL tells.
    fn unknown(account_id: Uuid, url: &str, state: &str) -> Self {
        let file_name = Url::parse(url)
            .ok()
            .and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(str::to_string)))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "download".into());
        DownloadInfo {
            id: Uuid::new_v4().to_string(),
            account_id: account_id.to_string(),
            url: url.to_string(),
            file_name,
            path: String::new(),
            state: state.to_string(),
        }
    }
}

fn push_download(queue: &mut DownloadQueue, url: String, pending: PendingDownload) {
    queue.entry(url).or_default().push_back(pending);
}

/// Takes the oldest unfinished download of `url`, if any.
fn pop_download(queue: &mut DownloadQueue, url: &str) -> Option<PendingDownload> {
    let list = queue.get_mut(url)?;
    let pending = list.pop_front();
    if list.is_empty() {
        queue.remove(url);
    }
    pending
}

/// Makes a downloaded file name safe to join onto a directory: no path separators,
/// control characters or surrounding blanks, and never empty, `.` or `..`.
fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !matches!(c, '/' | '\\') && !c.is_control()).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "download".into()
    } else {
        cleaned.to_string()
    }
}

/// `dir/name`, or `dir/stem (n).ext` with the smallest n that does not exist yet.
/// The extension is what follows the last dot; a leading dot (`.env`) is not one.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let name = sanitize_file_name(name);
    let first = dir.join(&name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    (1u32..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("ran out of file names")
}

/// wry has already renamed a clashing download to `base (n).rest`, splitting at the
/// first dot. Recovers the name the site suggested when that is what happened, i.e.
/// when the name has that shape and the unsuffixed file exists in `dir`.
fn undo_wry_rename(dir: &Path, name: &str) -> String {
    let (head, ext) = match name.find('.') {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    let original = head
        .strip_suffix(')')
        .and_then(|h| h.rsplit_once(" ("))
        .filter(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .map(|(base, _)| format!("{base}{ext}"));
    match original {
        Some(o) if !o.is_empty() && dir.join(&o).exists() => o,
        _ => name.to_string(),
    }
}

/// Where a requested download is saved: the Downloads folder (`dir`; wry's own choice
/// when unknown) under a name that does not overwrite anything.
fn download_destination(proposed: &Path, dir: Option<PathBuf>) -> PathBuf {
    let dir = dir
        .or_else(|| proposed.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let name = proposed.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match proposed.parent() {
        Some(wry_dir) => undo_wry_rename(wry_dir, &name),
        None => name,
    };
    unique_path(&dir, &name)
}

/// `path` without `.` / `..` components, resolved by text only (no file access).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Accepts only an existing file inside `dir` (symlinks and `..` resolved), so the UI
/// cannot be used to open arbitrary files.
fn check_in_dir(dir: &Path, path: &Path) -> Result<PathBuf, String> {
    const OUTSIDE: &str = "只能打开下载文件夹里的文件";
    if !path.is_absolute() {
        return Err(OUTSIDE.into());
    }
    let canon_dir = dir.canonicalize().unwrap_or_else(|_| normalize(dir));
    match path.canonicalize() {
        Ok(p) if p.starts_with(&canon_dir) && p != canon_dir => Ok(p),
        Ok(_) => Err(OUTSIDE.into()),
        Err(_) => {
            let p = normalize(path);
            if p.starts_with(normalize(dir)) || p.starts_with(&canon_dir) {
                Err("文件已被移动或删除".into())
            } else {
                Err(OUTSIDE.into())
            }
        }
    }
}

/// Saves page downloads to the Downloads folder and reports them to the UI. May run
/// on the main thread: only path arithmetic and an event, nothing that blocks.
fn handle_download(app: &AppHandle, account_id: Uuid, event: DownloadEvent<'_>) -> bool {
    let state = app.state::<AppState>();
    match event {
        DownloadEvent::Requested { url, destination } => {
            // wry's own de-duplication splits at the first dot (`a (1).v2.xlsx`); redo it.
            let path = download_destination(destination, app.path().download_dir().ok());
            *destination = path.clone();
            let pending = PendingDownload { id: Uuid::new_v4(), account_id, path };
            let info = pending.info(url.as_str(), "started");
            push_download(&mut state.downloads.lock().unwrap(), url.to_string(), pending);
            let _ = app.emit_to("main", "download", info);
            true
        }
        DownloadEvent::Finished { url, success, .. } => {
            // On macOS `path` is always None: look the download up by URL instead.
            let state_name = if success { "done" } else { "failed" };
            let pending = pop_download(&mut state.downloads.lock().unwrap(), url.as_str());
            let info = match pending {
                Some(p) => p.info(url.as_str(), state_name),
                None => {
                    eprintln!("download finished for an unknown request: {url}");
                    DownloadInfo::unknown(account_id, url.as_str(), state_name)
                }
            };
            let _ = app.emit_to("main", "download", info);
            true
        }
        _ => true,
    }
}

fn download_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().download_dir().map_err(|e| format!("找不到下载文件夹：{e}"))
}

/// Opens a downloaded file with its default app.
#[tauri::command]
fn open_download(app: AppHandle, path: String) -> Result<(), String> {
    let path = check_in_dir(&download_dir(&app)?, Path::new(&path))?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

/// Selects a downloaded file in Finder.
#[tauri::command]
fn reveal_download(app: AppHandle, path: String) -> Result<(), String> {
    let path = check_in_dir(&download_dir(&app)?, Path::new(&path))?;
    app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())
}

fn apply_visibility(app: &AppHandle) {
    let state = app.state::<AppState>();
    let active = *state.active.lock().unwrap();
    let tabs = state.tabs.lock().unwrap();
    for (account_id, t) in tabs.iter() {
        for tab in &t.tabs {
            if let Some(wv) = tab_webview(app, tab) {
                if Some(*account_id) == active && t.active == Some(*tab) {
                    let _ = wv.set_bounds(rect(*state.bounds.lock().unwrap()));
                    let _ = wv.show();
                } else {
                    let _ = wv.hide();
                }
            }
        }
    }
}

fn parse_id(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_sites(state: State<'_, AppState>) -> Vec<Site> {
    state.store.lock().unwrap().sites.clone()
}

#[tauri::command]
fn add_site(state: State<'_, AppState>, name: String, url: String) -> Result<Site, String> {
    let url = parse_url(&url)?;
    let name = name.trim();
    let site = Site {
        id: Uuid::new_v4(),
        name: if name.is_empty() {
            url.host_str().unwrap_or("网站").to_string()
        } else {
            name.to_string()
        },
        url: url.to_string(),
        accounts: vec![],
    };
    let mut store = state.store.lock().unwrap();
    store.sites.push(site.clone());
    state.save(&store)?;
    Ok(site)
}

#[tauri::command]
fn update_site(
    state: State<'_, AppState>,
    id: String,
    name: String,
    url: String,
) -> Result<(), String> {
    let id = parse_id(&id)?;
    let url = parse_url(&url)?;
    let mut store = state.store.lock().unwrap();
    let site = store.site_mut(id).ok_or("网站不存在")?;
    site.name = name.trim().to_string();
    site.url = url.to_string();
    state.save(&store)
}

#[tauri::command]
async fn delete_site(app: AppHandle, id: String) -> Result<(), String> {
    let id = parse_id(&id)?;
    let ids = {
        let state = app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let idx = store.sites.iter().position(|s| s.id == id).ok_or("网站不存在")?;
        let site = store.sites.remove(idx);
        state.save(&store)?;
        site.accounts.iter().map(|a| a.id).collect::<Vec<_>>()
    };
    delete_accounts_data(&app, ids).await;
    Ok(())
}

#[tauri::command]
fn add_account(state: State<'_, AppState>, site_id: String, name: String) -> Result<Account, String> {
    let site_id = parse_id(&site_id)?;
    let mut store = state.store.lock().unwrap();
    let site = store.site_mut(site_id).ok_or("网站不存在")?;
    let name = name.trim();
    let account = Account {
        id: Uuid::new_v4(),
        name: if name.is_empty() {
            format!("账号 {}", site.accounts.len() + 1)
        } else {
            name.to_string()
        },
        url: None,
        username: None,
        has_login: false,
    };
    site.accounts.push(account.clone());
    state.save(&store)?;
    Ok(account)
}

#[tauri::command]
fn rename_account(state: State<'_, AppState>, id: String, name: String) -> Result<(), String> {
    let id = parse_id(&id)?;
    let mut store = state.store.lock().unwrap();
    let acc = store.account_mut(id).ok_or("账号不存在")?;
    acc.name = name.trim().to_string();
    state.save(&store)
}

/// Sets the account's own home address; an empty one falls back to the site's URL.
#[tauri::command]
fn set_account_url(state: State<'_, AppState>, id: String, url: String) -> Result<(), String> {
    let id = parse_id(&id)?;
    let url = if url.trim().is_empty() { None } else { Some(parse_url(&url)?.to_string()) };
    let mut store = state.store.lock().unwrap();
    store.account_mut(id).ok_or("账号不存在")?.url = url;
    state.save(&store)
}

#[tauri::command]
async fn delete_account(app: AppHandle, id: String) -> Result<(), String> {
    let id = parse_id(&id)?;
    {
        let state = app.state::<AppState>();
        let mut store = state.store.lock().unwrap();
        let site = store
            .sites
            .iter_mut()
            .find(|s| s.accounts.iter().any(|a| a.id == id))
            .ok_or("账号不存在")?;
        site.accounts.retain(|a| a.id != id);
        state.save(&store)?;
    }
    delete_accounts_data(&app, vec![id]).await;
    Ok(())
}

/// Shows the account's active tab (opening a first tab on first use) and hides the others.
#[tauri::command]
async fn open_account(app: AppHandle, id: String) -> Result<(), String> {
    let id = parse_id(&id)?;
    let state = app.state::<AppState>();
    *state.active.lock().unwrap() = Some(id);
    let has_tabs = state.tabs.lock().unwrap().get(&id).is_some_and(|t| !t.tabs.is_empty());
    if has_tabs {
        emit_tabs(&app, id);
        apply_visibility(&app);
    } else {
        open_tab(&app, id, None)?;
    }
    Ok(())
}

#[tauri::command]
async fn new_tab(app: AppHandle, account_id: String) -> Result<String, String> {
    let account_id = parse_id(&account_id)?;
    *app.state::<AppState>().active.lock().unwrap() = Some(account_id);
    let id = open_tab(&app, account_id, None)?;
    // The new native webview takes keyboard focus; hand it back so the address bar
    // can take a pasted URL right away.
    if let Some(main) = app.get_webview("main") {
        let _ = main.set_focus();
    }
    Ok(id.to_string())
}

#[tauri::command]
fn select_tab(app: AppHandle, tab_id: String) -> Result<(), String> {
    let tab_id = parse_id(&tab_id)?;
    let state = app.state::<AppState>();
    let account_id = state.account_of_tab(tab_id).ok_or("标签页不存在")?;
    if let Some(t) = state.tabs.lock().unwrap().get_mut(&account_id) {
        t.active = Some(tab_id);
    }
    *state.active.lock().unwrap() = Some(account_id);
    emit_tabs(&app, account_id);
    apply_visibility(&app);
    Ok(())
}

/// Closes a tab; the neighbouring tab becomes active. Closing the last tab leaves the
/// account with no open page.
#[tauri::command]
fn close_tab(app: AppHandle, tab_id: String) -> Result<(), String> {
    let tab_id = parse_id(&tab_id)?;
    let state = app.state::<AppState>();
    let account_id = state.account_of_tab(tab_id).ok_or("标签页不存在")?;
    {
        let mut tabs = state.tabs.lock().unwrap();
        let t = tabs.get_mut(&account_id).unwrap();
        let idx = t.tabs.iter().position(|x| *x == tab_id).unwrap();
        t.tabs.remove(idx);
        if t.active == Some(tab_id) {
            t.active = t.tabs.get(idx.min(t.tabs.len().saturating_sub(1))).copied();
        }
        if t.tabs.is_empty() {
            tabs.remove(&account_id);
            let mut active = state.active.lock().unwrap();
            if *active == Some(account_id) {
                *active = None;
            }
        }
    }
    if let Some(wv) = tab_webview(&app, &tab_id) {
        let _ = wv.close();
    }
    emit_tabs(&app, account_id);
    apply_visibility(&app);
    Ok(())
}

#[tauri::command]
fn close_active(app: AppHandle) {
    *app.state::<AppState>().active.lock().unwrap() = None;
    apply_visibility(&app);
}

#[tauri::command]
fn set_content_bounds(app: AppHandle, mut bounds: Bounds) {
    let state = app.state::<AppState>();
    // Child webviews are positioned relative to the window's content view. On macOS
    // that view also spans the title bar while the HTML viewport starts below it,
    // so shift by the difference.
    if let Some(w) = app.get_window("main") {
        if let (Ok(size), Ok(scale)) = (w.inner_size(), w.scale_factor()) {
            let window_height = size.to_logical::<f64>(scale).height;
            bounds.y += (window_height - bounds.viewport_height).max(0.0);
        }
    }
    *state.bounds.lock().unwrap() = bounds;
    let tabs = state.tabs.lock().unwrap();
    for tab in tabs.values().flat_map(|t| t.tabs.iter()) {
        if let Some(wv) = tab_webview(&app, tab) {
            let _ = wv.set_bounds(rect(bounds));
        }
    }
}

/// Raises the sidebar UI above the account webviews while a dialog is open, and puts
/// it back below them afterwards. The page is never hidden, so nothing behind the
/// dialog changes: the UI's content area is transparent while raised (App.css).
#[tauri::command]
fn set_overlay(app: AppHandle, visible: bool) {
    *app.state::<AppState>().overlay.lock().unwrap() = visible;
    raise_ui(&app, visible);
}

/// Moves the main UI webview to the top (or bottom) of the window's child views.
#[cfg(target_os = "macos")]
fn raise_ui(app: &AppHandle, top: bool) {
    use objc2_app_kit::{NSView, NSWindowOrderingMode};
    let Some(main) = app.get_webview("main") else { return };
    let _ = main.with_webview(move |platform| {
        // SAFETY: on macOS `inner()` is the WKWebView (an NSView), and this closure runs
        // on the main thread.
        let view: &NSView = unsafe { &*platform.inner().cast() };
        let Some(parent) = (unsafe { view.superview() }) else { return };
        let order = if top { NSWindowOrderingMode::Above } else { NSWindowOrderingMode::Below };
        // Re-adding a subview moves it without unloading it.
        parent.addSubview_positioned_relativeTo(view, order, None);
    });
}

/// WKWebView bug (tauri-apps/wry#1763): the sidebar UI webview spans the whole window
/// under the account pages, and both webviews keep setting the mouse cursor while it
/// moves over a page, so it flickers between e.g. the page's hand and the UI's arrow.
/// Whenever the mouse moves, tell the UI webview to ignore mouse moves if the view
/// under the pointer is not part of it (an account page, or nothing). While a dialog
/// raises the UI above the pages, the pointer is over the UI and it works as usual.
#[cfg(target_os = "macos")]
fn fix_cursor_flicker(app: &AppHandle) {
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Bool, NSObjectProtocol};
    use objc2::{msg_send, sel};
    use objc2_app_kit::{NSEvent, NSEventMask, NSView};
    use std::cell::Cell;
    use std::ptr::NonNull;

    let Some(main) = app.get_webview("main") else { return };
    let _ = main.with_webview(|platform| {
        // SAFETY: on macOS `inner()` is the WKWebView, and this runs on the main thread.
        let Some(ui) = (unsafe { Retained::retain(platform.inner().cast::<NSView>()) }) else { return };
        // Private WKWebView switch; do nothing if a future WebKit drops it.
        let ignore_sel = sel!(_setIgnoresMouseMoveEvents:);
        if !ui.respondsToSelector(ignore_sel) {
            eprintln!("WKWebView has no _setIgnoresMouseMoveEvents:, cursor flicker fix disabled");
            return;
        }
        let ignoring = Cell::new(false);
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit passes a valid event; local monitors run on the main thread.
            let ev = unsafe { event.as_ref() };
            let ui_window = ui.window();
            let same_window = match (unsafe { ev.window(objc2::MainThreadMarker::new_unchecked()) }, &ui_window) {
                (Some(a), Some(b)) => Retained::as_ptr(&a) == Retained::as_ptr(b),
                _ => false,
            };
            if same_window {
                let over_ui = ui_window
                    .and_then(|w| w.contentView())
                    // The content view's superview uses window coordinates.
                    .and_then(|content| content.hitTest(ev.locationInWindow()))
                    .is_some_and(|hit| hit.isDescendantOf(&ui));
                if ignoring.get() == over_ui {
                    ignoring.set(!over_ui);
                    // SAFETY: checked above that the selector exists; it takes a BOOL.
                    let _: () = unsafe { msg_send![&*ui, _setIgnoresMouseMoveEvents: Bool::new(!over_ui)] };
                }
            }
            event.as_ptr()
        });
        let mask = NSEventMask::MouseMoved | NSEventMask::MouseEntered;
        // SAFETY: the handler returns the event it was given, unchanged.
        let monitor: Option<Retained<AnyObject>> =
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) };
        // Kept for the life of the app.
        std::mem::forget(monitor);
    });
}

/// Other platforms keep hiding the page behind dialogs.
#[cfg(not(target_os = "macos"))]
fn raise_ui(app: &AppHandle, top: bool) {
    let state = app.state::<AppState>();
    let active = *state.active.lock().unwrap();
    let tabs = state.tabs.lock().unwrap();
    let tab = active.and_then(|a| tabs.get(&a)).and_then(|t| t.active);
    if let Some(wv) = tab.and_then(|t| tab_webview(app, &t)) {
        let _ = if top { wv.hide() } else { wv.show() };
    }
}

#[tauri::command]
fn navigate_tab(app: AppHandle, tab_id: String, action: String, url: Option<String>) -> Result<(), String> {
    let tab_id = parse_id(&tab_id)?;
    let wv = tab_webview(&app, &tab_id).ok_or("标签页未打开")?;
    match action.as_str() {
        "back" => wv.eval("history.back()"),
        "forward" => wv.eval("history.forward()"),
        "reload" => wv.eval("location.reload()"),
        "stop" => wv.eval("window.stop()"),
        "home" | "go" => {
            let target = match url {
                Some(u) => parse_url(&u)?,
                None => {
                    let account_id = app
                        .state::<AppState>()
                        .account_of_tab(tab_id)
                        .ok_or("标签页不存在")?;
                    site_url_of(&app, account_id)?
                }
            };
            wv.navigate(target)
        }
        other => return Err(format!("unknown action {other}")),
    }
    .map_err(|e| e.to_string())
}

/// What the credential dialog can show without a Keychain prompt.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CredentialStatus {
    saved: bool,
    /// Set once the vault was unlocked this session.
    credential: Option<credentials::Credential>,
}

#[tauri::command]
fn credential_status(state: State<'_, AppState>, id: String) -> Result<CredentialStatus, String> {
    let id = parse_id(&id)?;
    if let Some(vault) = state.vault.lock().unwrap().as_ref() {
        let c = vault.get(&id).cloned();
        return Ok(CredentialStatus { saved: c.is_some(), credential: c });
    }
    let flagged = state.store.lock().unwrap().account_mut(id).is_some_and(|a| a.has_login);
    Ok(CredentialStatus { saved: flagged || credentials::exists(id)?, credential: None })
}

/// Reads the saved login. The first call of a session unlocks the Keychain vault,
/// which may ask for the Mac password once; every account is readable afterwards.
#[tauri::command]
fn get_credential(state: State<'_, AppState>, id: String) -> Result<Option<credentials::Credential>, String> {
    let id = parse_id(&id)?;
    unlock_vault(&state)?;
    let c = {
        let mut slot = state.vault.lock().unwrap();
        let vault = slot.as_mut().ok_or("钥匙串未解锁")?;
        take_legacy(vault, id, legacy_login)?
    };
    if let Some(vault) = c.moved.then(|| state.vault.lock().unwrap().clone()).flatten() {
        // Best effort: if this fails the old item is simply read again next time.
        if let Err(e) = credentials::save_vault(&vault) {
            eprintln!("failed to move saved login of account {id} into the vault: {e}");
        }
    }
    remember_login(&state, id, c.login.as_ref())?;
    Ok(c.login)
}

/// An account's item from before 0.9.0, if it has one. `exists` reads attributes
/// only, so accounts without one never cause a prompt.
fn legacy_login(id: Uuid) -> Result<Option<credentials::Credential>, String> {
    if credentials::exists(id)? { credentials::get(id) } else { Ok(None) }
}

struct VaultRead {
    login: Option<credentials::Credential>,
    /// Taken from a pre-0.9.0 item just now; the vault needs saving.
    moved: bool,
}

/// The account's login from the vault, or else from its pre-0.9.0 item, which is
/// then kept in the vault. Old items are only read here, one account at a time when
/// that account is opened: each asks for the Mac password separately (they were made
/// by an older build), so reading all of them at unlock meant one prompt per account.
fn take_legacy(
    vault: &mut credentials::Vault,
    id: Uuid,
    legacy: impl FnOnce(Uuid) -> Result<Option<credentials::Credential>, String>,
) -> Result<VaultRead, String> {
    if let Some(c) = vault.get(&id) {
        return Ok(VaultRead { login: Some(c.clone()), moved: false });
    }
    match legacy(id)?.filter(|c| !c.is_empty()) {
        Some(c) => {
            vault.insert(id, c.clone());
            Ok(VaultRead { login: Some(c), moved: true })
        }
        None => Ok(VaultRead { login: None, moved: false }),
    }
}

/// Mirrors what dialogs show without unlocking: the username and whether a login exists.
fn remember_login(state: &AppState, id: Uuid, c: Option<&credentials::Credential>) -> Result<(), String> {
    let username = c.map(|c| c.username.clone()).filter(|u| !u.is_empty());
    let has_login = c.is_some();
    let mut store = state.store.lock().unwrap();
    let Some(acc) = store.account_mut(id) else { return Ok(()) };
    if acc.username == username && acc.has_login == has_login {
        return Ok(());
    }
    acc.username = username;
    acc.has_login = has_login;
    state.save(&store)
}

/// Loads every saved login into memory, once per session: the one Keychain read
/// that can prompt, however many accounts there are. Nothing is written here, and
/// pre-0.9.0 items are left alone until their account is opened (see `take_legacy`).
/// Errors are not remembered, so a dismissed prompt can be retried.
fn unlock_vault(state: &AppState) -> Result<(), String> {
    // Held throughout, so two windows asking at once still prompt only once.
    let mut slot = state.vault.lock().unwrap();
    if slot.is_some() {
        return Ok(());
    }
    let ids = state.store.lock().unwrap().account_ids();
    let mut vault = credentials::load_vault()?;
    prune_vault(&mut vault, &ids);
    *slot = Some(vault);
    Ok(())
}

/// Drops logins of accounts deleted while the vault was locked (and empty ones).
/// Kept in memory only; written with the next save.
fn prune_vault(vault: &mut credentials::Vault, ids: &[Uuid]) {
    vault.retain(|id, c| ids.contains(id) && !c.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn cred(u: &str) -> credentials::Credential {
        credentials::Credential { username: u.into(), password: "p".into(), note: String::new() }
    }

    #[test]
    fn legacy_items_are_read_one_account_at_a_time() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut vault = credentials::Vault::new();
        vault.insert(a, cred("a"));
        let reads = Cell::new(0);
        let legacy = |id: Uuid| {
            reads.set(reads.get() + 1);
            Ok((id == b).then(|| cred("b")))
        };

        // In the vault: the old item is not even looked at.
        let r = take_legacy(&mut vault, a, legacy).unwrap();
        assert_eq!(r.login.unwrap().username, "a");
        assert!(!r.moved);
        assert_eq!(reads.get(), 0);

        // Opening b reads b's old item only, once, and keeps it in the vault.
        let r = take_legacy(&mut vault, b, legacy).unwrap();
        assert_eq!(r.login.unwrap().username, "b");
        assert!(r.moved);
        assert_eq!(reads.get(), 1);
        let r = take_legacy(&mut vault, b, legacy).unwrap();
        assert!(!r.moved);
        assert_eq!(reads.get(), 1);

        // No login anywhere.
        let r = take_legacy(&mut vault, c, legacy).unwrap();
        assert!(r.login.is_none() && !r.moved);
        assert!(!vault.contains_key(&c));
    }

    #[test]
    fn vault_drops_deleted_accounts() {
        let (a, gone) = (Uuid::new_v4(), Uuid::new_v4());
        let mut vault = credentials::Vault::new();
        vault.insert(a, cred("a"));
        vault.insert(gone, cred("x"));
        prune_vault(&mut vault, &[a]);
        assert_eq!(vault.len(), 1);
        assert!(vault.contains_key(&a));
    }

    #[test]
    fn legacy_read_error_is_reported_and_retried() {
        let id = Uuid::new_v4();
        let mut vault = credentials::Vault::new();
        assert!(take_legacy(&mut vault, id, |_| Err("denied".into())).is_err());
        assert!(vault.is_empty());
        let r = take_legacy(&mut vault, id, |_| Ok(Some(cred("x")))).unwrap();
        assert!(r.moved);
    }

    /// A fresh empty directory, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("web-manager-test-{}", Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir.canonicalize().unwrap())
        }

        fn touch(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, b"x").unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn name_of(p: &Path) -> String {
        p.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn unique_path_keeps_a_free_name() {
        let dir = TempDir::new();
        assert_eq!(unique_path(&dir.0, "a.pdf"), dir.0.join("a.pdf"));
    }

    #[test]
    fn unique_path_numbers_clashes() {
        let dir = TempDir::new();
        dir.touch("a.pdf");
        assert_eq!(name_of(&unique_path(&dir.0, "a.pdf")), "a (1).pdf");
        dir.touch("a (1).pdf");
        assert_eq!(name_of(&unique_path(&dir.0, "a.pdf")), "a (2).pdf");
    }

    #[test]
    fn unique_path_splits_at_the_last_dot() {
        let dir = TempDir::new();
        dir.touch("report.v2.xlsx");
        assert_eq!(name_of(&unique_path(&dir.0, "report.v2.xlsx")), "report.v2 (1).xlsx");
    }

    #[test]
    fn unique_path_without_extension_and_hidden_files() {
        let dir = TempDir::new();
        dir.touch("README");
        dir.touch(".env");
        assert_eq!(name_of(&unique_path(&dir.0, "README")), "README (1)");
        assert_eq!(name_of(&unique_path(&dir.0, ".env")), ".env (1)");
    }

    #[test]
    fn unique_path_stays_inside_dir() {
        let dir = TempDir::new();
        for name in ["../../etc/passwd", "a/b.txt", "", "..", ".", "  ", "a\\..\\b", "x\u{0}\n.txt"] {
            let p = unique_path(&dir.0, name);
            assert_eq!(p.parent(), Some(dir.0.as_path()), "{name:?} -> {p:?}");
            let n = name_of(&p);
            assert!(n != "." && n != ".." && !n.is_empty(), "{name:?} -> {n:?}");
        }
        assert_eq!(name_of(&unique_path(&dir.0, "")), "download");
        assert_eq!(name_of(&unique_path(&dir.0, "..")), "download");
        assert_eq!(name_of(&unique_path(&dir.0, "../../etc/passwd")), "....etcpasswd");
    }

    #[test]
    fn unique_path_keeps_chinese_and_spaces() {
        let dir = TempDir::new();
        assert_eq!(name_of(&unique_path(&dir.0, "销售 报表.xlsx")), "销售 报表.xlsx");
        dir.touch("销售 报表.xlsx");
        assert_eq!(name_of(&unique_path(&dir.0, "销售 报表.xlsx")), "销售 报表 (1).xlsx");
    }

    #[test]
    fn destination_undoes_wry_first_dot_rename() {
        let dir = TempDir::new();
        dir.touch("report.v2.xlsx");
        // What wry proposes when report.v2.xlsx already exists.
        let proposed = dir.0.join("report (1).v2.xlsx");
        let p = download_destination(&proposed, Some(dir.0.clone()));
        assert_eq!(name_of(&p), "report.v2 (1).xlsx");
        // A site that really names its file like that keeps the name.
        let p = download_destination(&dir.0.join("photo (3).jpg"), Some(dir.0.clone()));
        assert_eq!(name_of(&p), "photo (3).jpg");
        // Unknown Downloads folder: wry's directory is used.
        let p = download_destination(&dir.0.join("new.txt"), None);
        assert_eq!(p, dir.0.join("new.txt"));
    }

    #[test]
    fn download_queue_is_first_in_first_out() {
        let mut q = DownloadQueue::new();
        let account = Uuid::new_v4();
        let url = "https://example.com/a.zip";
        let first = PendingDownload { id: Uuid::new_v4(), account_id: account, path: "/d/a.zip".into() };
        let second = PendingDownload { id: Uuid::new_v4(), account_id: account, path: "/d/a (1).zip".into() };
        push_download(&mut q, url.into(), first.clone());
        push_download(&mut q, url.into(), second.clone());
        assert_eq!(pop_download(&mut q, url), Some(first));
        assert_eq!(pop_download(&mut q, url), Some(second));
        assert_eq!(pop_download(&mut q, url), None);
        assert!(q.is_empty());
        assert_eq!(pop_download(&mut q, "https://unknown.example/x"), None);
        let info = DownloadInfo::unknown(account, "https://unknown.example/files/x.pdf?y=1", "done");
        assert_eq!(info.file_name, "x.pdf");
        assert_eq!(DownloadInfo::unknown(account, "blob:xyz", "failed").file_name, "download");
    }

    #[test]
    fn download_info_fields() {
        let p = PendingDownload { id: Uuid::new_v4(), account_id: Uuid::new_v4(), path: "/d/销售 报表.xlsx".into() };
        let v = serde_json::to_value(p.info("https://e.com/r", "started")).unwrap();
        assert_eq!(v["fileName"], "销售 报表.xlsx");
        assert_eq!(v["accountId"], p.account_id.to_string());
        assert_eq!(v["path"], "/d/销售 报表.xlsx");
        assert_eq!(v["state"], "started");
    }

    #[test]
    fn open_only_files_in_the_download_dir() {
        let root = TempDir::new();
        let downloads = root.0.join("Downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let inside = downloads.join("a.pdf");
        std::fs::write(&inside, b"x").unwrap();
        let outside = root.touch("secret.txt");

        assert_eq!(check_in_dir(&downloads, &inside), Ok(inside.clone()));
        assert!(check_in_dir(&downloads, &outside).is_err());
        assert!(check_in_dir(&downloads, &downloads.join("../secret.txt")).is_err());
        assert!(check_in_dir(&downloads, &downloads.join("../../../etc/passwd")).is_err());
        assert!(check_in_dir(&downloads, Path::new("a.pdf")).is_err());
        assert!(check_in_dir(&downloads, &downloads).is_err());
        assert!(check_in_dir(&downloads, Path::new("/etc/passwd")).is_err());
        // A symlink inside the folder pointing out of it is refused too.
        #[cfg(unix)]
        {
            let link = downloads.join("link.txt");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(check_in_dir(&downloads, &link).is_err());
        }
        // Deleted file: a clear message, not a crash.
        assert_eq!(check_in_dir(&downloads, &downloads.join("gone.pdf")), Err("文件已被移动或删除".to_string()));
        // ...but a missing file outside is still "outside".
        assert_ne!(check_in_dir(&downloads, &downloads.join("../gone.pdf")), Err("文件已被移动或删除".to_string()));
    }
}

#[tauri::command]
fn set_credential(
    state: State<'_, AppState>,
    id: String,
    credential: credentials::Credential,
) -> Result<(), String> {
    let id = parse_id(&id)?;
    state.store.lock().unwrap().account_mut(id).ok_or("账号不存在")?;
    unlock_vault(&state)?;
    {
        let mut slot = state.vault.lock().unwrap();
        let vault = slot.as_mut().ok_or("钥匙串未解锁")?;
        let mut next = vault.clone();
        if credential.is_empty() {
            next.remove(&id);
        } else {
            next.insert(id, credential.clone());
        }
        credentials::save_vault(&next)?;
        *vault = next;
    }
    // Cleared: also remove a pre-0.9.0 item, or `take_legacy` would bring it back.
    if credential.is_empty() && credentials::exists(id)? {
        credentials::delete(id)?;
    }
    remember_login(&state, id, (!credential.is_empty()).then_some(&credential))
}

fn peek_label(account_id: &Uuid) -> String {
    format!("peek-{}", account_id.simple())
}

/// Opens (or brings forward) a small window showing an account's saved login with
/// copy buttons. It is a window of its own, so it floats above the page, can be
/// dragged anywhere, and the page's login form stays clickable while it is open.
#[tauri::command]
async fn open_credential_peek(app: AppHandle, id: String) -> Result<(), String> {
    let account_id = parse_id(&id)?;
    let title = {
        let state = app.state::<AppState>();
        let store = state.store.lock().unwrap();
        let site = store
            .sites
            .iter()
            .find(|s| s.accounts.iter().any(|a| a.id == account_id))
            .ok_or("账号不存在")?;
        let acc = site.accounts.iter().find(|a| a.id == account_id).ok_or("账号不存在")?;
        format!("{} · {}", site.name, acc.name)
    };
    let label = peek_label(&account_id);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        let _ = w.show();
        return w.set_focus().map_err(|e| e.to_string());
    }
    let url = format!("index.html?peek={account_id}");
    WebviewWindowBuilder::new(&app, label, WebviewUrl::App(url.into()))
        .title(title)
        .inner_size(320.0, 150.0)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .always_on_top(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let store_path = data_dir.join("sites.json");
            let profiles_dir = data_dir.join("profiles");
            let store = Store::load(&store_path);
            let pending = store.pending_purge.clone();
            app.manage(AppState {
                store: Mutex::new(store),
                store_path,
                profiles_dir,
                active: Mutex::new(None),
                tabs: Mutex::new(HashMap::new()),
                bounds: Mutex::new(Bounds::default()),
                overlay: Mutex::new(false),
                vault: Mutex::new(None),
                downloads: Mutex::new(HashMap::new()),
            });

            // The UI is drawn over the account page while a dialog is open, with its
            // content area transparent (see `set_overlay`); the HTML paints its own
            // background otherwise.
            if let Some(main) = app.get_webview("main") {
                main.set_background_color(Some(tauri::webview::Color(0, 0, 0, 0)))?;
            }
            #[cfg(target_os = "macos")]
            fix_cursor_flicker(app.handle());

            // Retry data deletions that failed last time (e.g. files locked on Windows).
            if !pending.is_empty() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let mut done = vec![];
                    for id in pending {
                        if purge_account_data(&handle, id).await.is_ok() {
                            done.push(id);
                        }
                    }
                    let state = handle.state::<AppState>();
                    let mut store = state.store.lock().unwrap();
                    store.pending_purge.retain(|id| !done.contains(id));
                    let _ = state.save(&store);
                });
            }
            Ok(())
        })
        // Small credential windows must not keep the app running once the main
        // window is gone.
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, WindowEvent::Destroyed) {
                window.app_handle().exit(0);
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_sites,
            add_site,
            update_site,
            delete_site,
            add_account,
            rename_account,
            set_account_url,
            delete_account,
            open_account,
            close_active,
            set_content_bounds,
            set_overlay,
            new_tab,
            select_tab,
            close_tab,
            navigate_tab,
            get_credential,
            set_credential,
            credential_status,
            open_download,
            reveal_download,
            open_credential_peek,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
