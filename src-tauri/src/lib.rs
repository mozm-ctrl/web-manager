mod credentials;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Rect, State, Url, Webview,
    WebviewBuilder, WebviewUrl,
};
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
    /// When true (e.g. a dialog is open in the sidebar UI) the active account webview is hidden.
    overlay: Mutex<bool>,
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
    loading: bool,
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
        if let Err(e) = credentials::delete(id) {
            eprintln!("failed to delete saved login for account {id}: {e}");
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

fn emit_page(app: &AppHandle, account_id: Uuid, tab_id: Uuid, url: &Url, title: Option<String>, loading: bool) {
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
    parse_url(&site.url)
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

    #[allow(unused_mut)]
    let mut builder = WebviewBuilder::new(&label, WebviewUrl::External(url))
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
            emit_page(&app_for_load, account_id, tab_id, payload.url(), None, loading);
        })
        .on_document_title_changed(move |wv, title| {
            if let Ok(url) = wv.url() {
                emit_page(&app_for_title, account_id, tab_id, &url, Some(title), false);
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
        });

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
    emit_tabs(app, account_id);
    apply_visibility(app);
    Ok(tab_id)
}

fn apply_visibility(app: &AppHandle) {
    let state = app.state::<AppState>();
    let active = *state.active.lock().unwrap();
    let overlay = *state.overlay.lock().unwrap();
    let tabs = state.tabs.lock().unwrap();
    for (account_id, t) in tabs.iter() {
        for tab in &t.tabs {
            if let Some(wv) = tab_webview(app, tab) {
                if Some(*account_id) == active && t.active == Some(*tab) && !overlay {
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
    open_tab(&app, account_id, None).map(|id| id.to_string())
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

#[tauri::command]
fn set_overlay(app: AppHandle, visible: bool) {
    *app.state::<AppState>().overlay.lock().unwrap() = visible;
    apply_visibility(&app);
}

#[tauri::command]
fn navigate_tab(app: AppHandle, tab_id: String, action: String, url: Option<String>) -> Result<(), String> {
    let tab_id = parse_id(&tab_id)?;
    let wv = tab_webview(&app, &tab_id).ok_or("标签页未打开")?;
    match action.as_str() {
        "back" => wv.eval("history.back()"),
        "forward" => wv.eval("history.forward()"),
        "reload" => wv.eval("location.reload()"),
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

#[tauri::command]
fn get_credential(id: String) -> Result<Option<credentials::Credential>, String> {
    credentials::get(parse_id(&id)?)
}

#[tauri::command]
fn set_credential(
    state: State<'_, AppState>,
    id: String,
    credential: credentials::Credential,
) -> Result<(), String> {
    let id = parse_id(&id)?;
    state.store.lock().unwrap().account_mut(id).ok_or("账号不存在")?;
    credentials::set(id, &credential)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
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
            });

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
        .invoke_handler(tauri::generate_handler![
            list_sites,
            add_site,
            update_site,
            delete_site,
            add_account,
            rename_account,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
