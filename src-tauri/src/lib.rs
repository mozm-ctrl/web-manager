mod credentials;
mod store;

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
    active: Mutex<Option<Uuid>>,
    bounds: Mutex<Bounds>,
    /// When true (e.g. a dialog is open in the sidebar UI) the active account webview is hidden.
    overlay: Mutex<bool>,
}

impl AppState {
    fn save(&self, store: &Store) -> Result<(), String> {
        store.save(&self.store_path)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    account_id: String,
    url: String,
    title: Option<String>,
    loading: bool,
}

fn webview_label(account_id: &Uuid) -> String {
    format!("acc-{}", account_id.simple())
}

fn account_webview(app: &AppHandle, account_id: &Uuid) -> Option<Webview> {
    app.get_webview(&webview_label(account_id))
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
    if let Some(wv) = account_webview(app, &account_id) {
        wv.close().map_err(|e| e.to_string())?;
    }

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

fn emit_page(app: &AppHandle, account_id: Uuid, url: &Url, title: Option<String>, loading: bool) {
    let _ = app.emit_to(
        "main",
        "account-page",
        PageInfo {
            account_id: account_id.to_string(),
            url: url.to_string(),
            title,
            loading,
        },
    );
}

fn build_account_webview(app: &AppHandle, site: &Site, account: &Account) -> Result<Webview, String> {
    let state = app.state::<AppState>();
    let url = parse_url(&site.url)?;
    let account_id = account.id;
    let label = webview_label(&account_id);

    let app_for_popup = app.clone();
    let popup_label = label.clone();
    let app_for_load = app.clone();
    let app_for_title = app.clone();

    #[allow(unused_mut)]
    let mut builder = WebviewBuilder::new(&label, WebviewUrl::External(url))
        .on_page_load(move |wv, payload| {
            let loading = matches!(payload.event(), PageLoadEvent::Started);
            let _ = wv;
            emit_page(&app_for_load, account_id, payload.url(), None, loading);
        })
        .on_document_title_changed(move |wv, title| {
            if let Ok(url) = wv.url() {
                emit_page(&app_for_title, account_id, &url, Some(title), false);
            }
        })
        // Open target=_blank / window.open links in the same account webview so they
        // keep this account's cookies.
        .on_new_window(move |url, _features| {
            if let Some(wv) = app_for_popup.get_webview(&popup_label) {
                let _ = wv.navigate(url);
            }
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
        .map_err(|e| e.to_string())
}

fn apply_visibility(app: &AppHandle) {
    let state = app.state::<AppState>();
    let active = *state.active.lock().unwrap();
    let overlay = *state.overlay.lock().unwrap();
    let store = state.store.lock().unwrap();
    for site in &store.sites {
        for acc in &site.accounts {
            if let Some(wv) = account_webview(app, &acc.id) {
                if Some(acc.id) == active && !overlay {
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

/// Shows the account's webview (creating it on first use) and hides the others.
#[tauri::command]
async fn open_account(app: AppHandle, id: String) -> Result<(), String> {
    let id = parse_id(&id)?;
    let state = app.state::<AppState>();
    if account_webview(&app, &id).is_none() {
        let (site, account) = {
            let store = state.store.lock().unwrap();
            let site = store
                .sites
                .iter()
                .find(|s| s.accounts.iter().any(|a| a.id == id))
                .ok_or("账号不存在")?;
            let account = site.accounts.iter().find(|a| a.id == id).unwrap().clone();
            (site.clone(), account)
        };
        build_account_webview(&app, &site, &account)?;
    }
    *state.active.lock().unwrap() = Some(id);
    if let Some(wv) = account_webview(&app, &id) {
        let _ = wv.set_bounds(rect(*state.bounds.lock().unwrap()));
        if let Ok(url) = wv.url() {
            emit_page(&app, id, &url, None, false);
        }
    }
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
    let active = *state.active.lock().unwrap();
    if let Some(wv) = active.and_then(|id| account_webview(&app, &id)) {
        let _ = wv.set_bounds(rect(bounds));
    }
}

#[tauri::command]
fn set_overlay(app: AppHandle, visible: bool) {
    *app.state::<AppState>().overlay.lock().unwrap() = visible;
    apply_visibility(&app);
}

#[tauri::command]
fn navigate_account(app: AppHandle, id: String, action: String, url: Option<String>) -> Result<(), String> {
    let id = parse_id(&id)?;
    let wv = account_webview(&app, &id).ok_or("账号页面未打开")?;
    match action.as_str() {
        "back" => wv.eval("history.back()"),
        "forward" => wv.eval("history.forward()"),
        "reload" => wv.eval("location.reload()"),
        "home" | "go" => {
            let target = match url {
                Some(u) => parse_url(&u)?,
                None => {
                    let store = app.state::<AppState>().store.lock().unwrap().clone();
                    let site = store
                        .sites
                        .iter()
                        .find(|s| s.accounts.iter().any(|a| a.id == id))
                        .ok_or("账号不存在")?;
                    parse_url(&site.url)?
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
            navigate_account,
            get_credential,
            set_credential,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
