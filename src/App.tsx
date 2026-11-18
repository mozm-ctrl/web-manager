import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { api, type Account, type Credential, type PageEvent, type PageInfo, type Site, type TabsInfo } from "./api";
import { Icon } from "./Icon";
import "./App.css";

type DialogState =
  | { kind: "addSite" }
  | { kind: "editSite"; site: Site }
  | { kind: "addAccount"; site: Site }
  | { kind: "renameAccount"; account: Account }
  | { kind: "credential"; account: Account; site: Site }
  | { kind: "confirm"; message: string; onConfirm: () => Promise<void> }
  /** `update` is null when there is no newer version, or the check failed (`error`). */
  | { kind: "update"; update: Update | null; error?: string };

const SIDEBAR_MIN = 180;
const SIDEBAR_MAX = 520;
// macOS draws the traffic lights over our own title bar (titleBarStyle "Overlay").
const IS_MAC = navigator.userAgent.includes("Mac");

function App() {
  const [sites, setSites] = useState<Site[]>([]);
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [activeId, setActiveId] = useState<string | null>(null);
  // Keyed by tab id.
  const [pages, setPages] = useState<Record<string, PageInfo>>({});
  // Keyed by account id.
  const [tabs, setTabs] = useState<Record<string, TabsInfo>>({});
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [resizing, setResizing] = useState(false);
  const [sidebarWidth, setSidebarWidth] = useState(() => {
    const saved = Number(localStorage.getItem("sidebarWidth"));
    return saved >= SIDEBAR_MIN && saved <= SIDEBAR_MAX ? saved : 260;
  });
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => localStorage.getItem("sidebarCollapsed") === "1");
  const [dark, setDark] = useState(() => localStorage.getItem("theme") === "dark");
  const contentRef = useRef<HTMLDivElement>(null);
  // Per-tab timeouts that clear a loading state the engine never finished (see setLoading).
  const loadTimers = useRef<Record<string, number>>({});

  const toggleSidebar = () =>
    setSidebarCollapsed((c) => {
      localStorage.setItem("sidebarCollapsed", c ? "0" : "1");
      return !c;
    });

  // The window theme also darkens the native title bar and traffic lights.
  useEffect(() => {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    localStorage.setItem("theme", dark ? "dark" : "light");
    getCurrentWindow()
      .setTheme(dark ? "dark" : "light")
      .catch(() => {});
  }, [dark]);

  const refresh = useCallback(async () => setSites(await api.listSites()), []);

  // Checked once per launch; a failed check stays quiet until the user asks (检查更新).
  const [version, setVersion] = useState("");
  useEffect(() => {
    getVersion().then(setVersion);
    check()
      .then((update) => update && setDialog((d) => d ?? { kind: "update", update }))
      .catch((e) => console.warn("update check failed:", e));
  }, []);

  const checkForUpdate = () =>
    check()
      .then((update) => setDialog({ kind: "update", update }))
      .catch((e) => setDialog({ kind: "update", update: null, error: String(e) }));

  const run = useCallback(async (fn: () => Promise<unknown>) => {
    try {
      setError(null);
      await fn();
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
    const unlisten = listen<PageEvent>("account-page", ({ payload }) => {
      if (payload.loading === false) window.clearTimeout(loadTimers.current[payload.tabId]);
      setPages((p) => ({
        ...p,
        [payload.tabId]: {
          ...payload,
          title: payload.title ?? p[payload.tabId]?.title ?? null,
          loading: payload.loading ?? p[payload.tabId]?.loading ?? false,
        },
      }));
    });
    const unlistenTabs = listen<TabsInfo>("account-tabs", ({ payload }) => {
      setTabs((t) => ({ ...t, [payload.accountId]: payload }));
      // Closing an account's last tab leaves nothing to show.
      if (payload.tabs.length === 0) {
        setActiveId((cur) => (cur === payload.accountId ? null : cur));
      }
    });
    return () => {
      unlisten.then((f) => f());
      unlistenTabs.then((f) => f());
    };
  }, [refresh]);

  // Account webviews are native children of the window positioned over this element.
  useLayoutEffect(() => {
    const el = contentRef.current;
    if (!el) return;
    const report = () => {
      const r = el.getBoundingClientRect();
      api.setContentBounds({
        x: r.left,
        y: r.top,
        width: r.width,
        height: r.height,
        viewportHeight: window.innerHeight,
      });
    };
    report();
    const ro = new ResizeObserver(report);
    ro.observe(el);
    window.addEventListener("resize", report);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", report);
    };
  }, []);

  // Native webviews sit above the HTML UI and would cover a dialog. While one is open,
  // the UI is raised above them instead (set_overlay) with the content area left
  // transparent, so the live page stays on screen behind the dialog — nothing is
  // hidden or swapped, which is what used to flash. Resizing keeps the normal order:
  // a drag that starts on the splitter keeps its pointer events even over the webview.
  // `raised` is false until the UI is on top, so the dialog never shows half over the
  // sidebar and half under the page.
  const [raised, setRaised] = useState(false);
  const dialogOpen = dialog !== null;
  useEffect(() => {
    if (!dialogOpen) {
      setRaised(false);
      // Back below the page first, then make the content area opaque again.
      api.setOverlay(false).finally(() => document.documentElement.classList.remove("over-page"));
      return;
    }
    // Transparent first, then raise, so the raised UI never covers the page.
    document.documentElement.classList.add("over-page");
    let cancelled = false;
    (async () => {
      await nextPaint();
      if (cancelled) return;
      await api.setOverlay(true).catch(() => {});
      if (!cancelled) setRaised(true);
    })();
    return () => {
      cancelled = true;
    };
  }, [dialogOpen]);

  // Set when + is clicked so the new tab's address bar takes focus with its URL selected.
  const [focusAddressFor, setFocusAddressFor] = useState<string | null>(null);

  const openAccount = (id: string) =>
    run(async () => {
      await api.openAccount(id);
      setActiveId(id);
    });

  const active = sites
    .flatMap((s) => s.accounts.map((a) => ({ site: s, account: a })))
    .find((x) => x.account.id === activeId);
  const accountTabs = activeId ? tabs[activeId] : undefined;
  const activeTab = accountTabs?.activeTab ?? null;
  const page = activeTab ? pages[activeTab] : undefined;
  // The engine reports a load only once the server first responds, so mark the tab
  // as loading as soon as we start one; a page that never responds is cleared after
  // a while so the spinner cannot spin forever.
  const setLoading = useCallback((tabId: string, loading: boolean, accountId?: string) => {
    window.clearTimeout(loadTimers.current[tabId]);
    if (loading) loadTimers.current[tabId] = window.setTimeout(() => setLoading(tabId, false), 30000);
    setPages((p) => {
      if (p[tabId]) return { ...p, [tabId]: { ...p[tabId], loading } };
      // A brand-new tab has no page info yet.
      if (!accountId) return p;
      return { ...p, [tabId]: { accountId, tabId, url: "", title: null, loading } };
    });
  }, []);

  const nav = (action: "back" | "forward" | "reload" | "stop" | "home" | "go", url?: string) =>
    activeTab &&
    run(async () => {
      if (action !== "stop") setLoading(activeTab, true);
      try {
        await api.navigate(activeTab, action, url);
      } catch (e) {
        setLoading(activeTab, false);
        throw e;
      } finally {
        if (action === "stop") setLoading(activeTab, false);
      }
    });

  return (
    <div className={`app ${IS_MAC ? "mac" : ""} ${sidebarCollapsed ? "sidebar-collapsed" : ""}`}>
      <aside className="sidebar" style={{ width: sidebarWidth }}>
        <div className="titlebar" data-tauri-drag-region />
        <div className="sidebar-header">
          <span>网站</span>
          <button className="icon" title="添加网站" aria-label="添加网站" onClick={() => setDialog({ kind: "addSite" })}>
            <Icon name="plus" />
          </button>
        </div>
        <div className="site-list">
          {sites.length === 0 && <div className="empty">点击右上角 ＋ 添加第一个网站</div>}
          {sites.map((site) => (
            <div key={site.id} className="site">
              <div
                className="site-row"
                onClick={() => setCollapsed((c) => ({ ...c, [site.id]: !c[site.id] }))}
              >
                <span className="caret">
                  <Icon name={collapsed[site.id] ? "chevronRight" : "chevronDown"} size={14} />
                </span>
                <button
                  className="site-name"
                  title={site.url}
                  aria-label={`${collapsed[site.id] ? "展开" : "收起"} ${site.name}`}
                  aria-expanded={!collapsed[site.id]}
                >
                  {site.name}
                </button>
                <span className="row-actions" onClick={(e) => e.stopPropagation()}>
                  <button className="icon" title="新增账号" aria-label={`给 ${site.name} 新增账号`} onClick={() => setDialog({ kind: "addAccount", site })}>
                    <Icon name="plus" />
                  </button>
                  <button className="icon" title="编辑网站" aria-label={`编辑网站 ${site.name}`} onClick={() => setDialog({ kind: "editSite", site })}>
                    <Icon name="edit" />
                  </button>
                  <button
                    className="icon danger"
                    title="删除网站"
                    aria-label={`删除网站 ${site.name}`}
                    onClick={() =>
                      setDialog({
                        kind: "confirm",
                        message: `删除网站「${site.name}」及其 ${site.accounts.length} 个账号？所有账号的登录数据会被清除。`,
                        onConfirm: async () => {
                          await api.deleteSite(site.id);
                          if (site.accounts.some((a) => a.id === activeId)) setActiveId(null);
                          await refresh();
                        },
                      })
                    }
                  >
                    <Icon name="close" />
                  </button>
                </span>
              </div>
              {!collapsed[site.id] && (
                <div className="accounts">
                  {site.accounts.length === 0 && <div className="empty small">点网站右侧 ＋ 新增账号</div>}
                  {site.accounts.map((acc) => (
                    <div
                      key={acc.id}
                      className={`account-row ${acc.id === activeId ? "active" : ""}`}
                      onClick={() => openAccount(acc.id)}
                    >
                      <button className="account-name" aria-label={`打开 ${acc.name}`}>
                        {acc.name}
                      </button>
                      <span className="row-actions" onClick={(e) => e.stopPropagation()}>
                        <button
                          className="icon"
                          title="账号密码"
                          aria-label={`账号密码 ${acc.name}`}
                          onClick={() => setDialog({ kind: "credential", account: acc, site })}
                        >
                          <Icon name="key" />
                        </button>
                        <button className="icon" title="重命名" aria-label={`重命名 ${acc.name}`} onClick={() => setDialog({ kind: "renameAccount", account: acc })}>
                          <Icon name="edit" />
                        </button>
                        <button
                          className="icon danger"
                          title="删除账号"
                          aria-label={`删除账号 ${acc.name}`}
                          onClick={() =>
                            setDialog({
                              kind: "confirm",
                              message: `删除账号「${acc.name}」？它的 Cookie 和登录状态会被清除。`,
                              onConfirm: async () => {
                                await api.deleteAccount(acc.id);
                                if (acc.id === activeId) setActiveId(null);
                                await refresh();
                              },
                            })
                          }
                        >
                          <Icon name="close" />
                        </button>
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
        {error && (
          <div className="error" onClick={() => setError(null)}>
            {error}
          </div>
        )}
        <div className="sidebar-footer">
          <button className="check-update" title="检查是否有新版本" onClick={checkForUpdate}>
            检查更新{version && <span className="version">v{version}</span>}
          </button>
        </div>
      </aside>
      <div
        className={`splitter ${resizing ? "dragging" : ""}`}
        role="separator"
        aria-orientation="vertical"
        aria-label="拖动调整侧栏宽度"
        title="拖动调整宽度，双击恢复默认"
        onDoubleClick={() => {
          setSidebarWidth(260);
          localStorage.setItem("sidebarWidth", "260");
        }}
        onPointerDown={(e) => {
          e.preventDefault();
          const startX = e.clientX;
          const startWidth = sidebarWidth;
          let width = startWidth;
          setResizing(true);
          const move = (ev: PointerEvent) => {
            width = Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, startWidth + ev.clientX - startX));
            setSidebarWidth(width);
          };
          const up = () => {
            setResizing(false);
            window.removeEventListener("pointermove", move);
            window.removeEventListener("pointerup", up);
            window.removeEventListener("pointercancel", up);
            localStorage.setItem("sidebarWidth", String(width));
          };
          window.addEventListener("pointermove", move);
          window.addEventListener("pointerup", up);
          window.addEventListener("pointercancel", up);
        }}
      />

      <main className="main">
        <div className="titlebar" data-tauri-drag-region>
          <span className="window-title" data-tauri-drag-region title={page?.title ?? ""}>
            {active
              ? `${active.site.name} · ${active.account.name}${page?.loading ? " · 加载中" : ""}`
              : "网页管理器"}
          </span>
        </div>
        <div className="toolbar">
          <button
            className="icon"
            title={sidebarCollapsed ? "展开侧栏" : "收起侧栏"}
            aria-label={sidebarCollapsed ? "展开侧栏" : "收起侧栏"}
            aria-expanded={!sidebarCollapsed}
            onClick={toggleSidebar}
          >
            <Icon name="sidebar" />
          </button>
          {active ? (
            <>
              <button className="icon" title="后退" aria-label="后退" onClick={() => nav("back")}>
                <Icon name="back" />
              </button>
              <button className="icon" title="前进" aria-label="前进" onClick={() => nav("forward")}>
                <Icon name="forward" />
              </button>
              {page?.loading ? (
                <button className="icon" title="停止加载" aria-label="停止加载" onClick={() => nav("stop")}>
                  <Icon name="close" />
                </button>
              ) : (
                <button className="icon" title="刷新" aria-label="刷新" onClick={() => nav("reload")}>
                  <Icon name="reload" />
                </button>
              )}
              <button className="icon" title="回到首页" aria-label="回到首页" onClick={() => nav("home")}>
                <Icon name="home" />
              </button>
              <button
                className="icon"
                title="查看此账号保存的账号密码"
                aria-label="查看账号密码"
                onClick={() => setDialog({ kind: "credential", account: active.account, site: active.site })}
              >
                <Icon name="key" />
              </button>
              <AddressBar
                key={activeTab ?? active.account.id}
                url={page?.url || active.account.url || active.site.url}
                autoFocus={activeTab !== null && activeTab === focusAddressFor}
                onGo={(url) => nav("go", url)}
              />
            </>
          ) : (
            <span className="hint">选择左侧的账号打开网站</span>
          )}
          <button
            className="icon theme-toggle"
            title={dark ? "切换到白天模式" : "切换到夜间模式"}
            aria-label={dark ? "切换到白天模式" : "切换到夜间模式"}
            onClick={() => setDark((d) => !d)}
          >
            <Icon name={dark ? "sun" : "moon"} />
          </button>
        </div>
        {active && (
          <div className="tabbar">
            {(accountTabs?.tabs ?? []).map((tabId) => {
              const p = pages[tabId];
              const label = p?.title || (p?.url ? hostOf(p.url) : "新标签页");
              return (
                <div key={tabId} className={`tab ${tabId === activeTab ? "active" : ""}`} title={p?.url}>
                  <button
                    className="tab-title"
                    aria-label={`标签 ${label}`}
                    onClick={() => run(() => api.selectTab(tabId))}
                  >
                    {p?.loading && <span className="spinner" role="status" aria-label="加载中" />}
                    <span className="tab-label">{label}</span>
                  </button>
                  <button
                    className="icon tab-close"
                    title="关闭标签页"
                    aria-label={`关闭标签 ${label}`}
                    onClick={() => run(() => api.closeTab(tabId))}
                  >
                    <Icon name="close" />
                  </button>
                </div>
              );
            })}
            <button
              className="icon"
              title="新建标签页"
              aria-label="新建标签页"
              onClick={() =>
                run(async () => {
                  const tabId = await api.newTab(active.account.id);
                  setLoading(tabId, true, active.account.id);
                  setFocusAddressFor(tabId);
                })
              }
            >
              <Icon name="plus" />
            </button>
          </div>
        )}
        <div className="content" ref={contentRef}>
          {!active && (
            <div className="placeholder">
              <h2>网页管理器</h2>
              <p>每个账号使用独立的浏览器数据（Cookie、LocalStorage 等），同一网站可同时登录多个账号。</p>
            </div>
          )}
        </div>
      </main>

      {resizing && <div className="resize-overlay" />}
      <div className={raised ? "" : "dialog-pending"}>
        {dialog?.kind === "update" ? (
          <UpdateDialog update={dialog.update} error={dialog.error} version={version} onClose={() => setDialog(null)} />
        ) : dialog?.kind === "credential" ? (
          <CredentialDialog
            account={dialog.account}
            site={dialog.site}
            onClose={() => setDialog(null)}
            onSaved={refresh}
          />
        ) : (
          dialog && <Dialog state={dialog} onClose={() => setDialog(null)} onDone={refresh} run={run} />
        )}
      </div>
    </div>
  );
}

/** Resolves once the frame showing the current DOM changes has been painted. */
function nextPaint() {
  return new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
}

function hostOf(url: string) {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function AddressBar({ url, autoFocus, onGo }: { url: string; autoFocus: boolean; onGo: (url: string) => void }) {
  const [value, setValue] = useState(url);
  const inputRef = useRef<HTMLInputElement>(null);
  // Set while the user has typed something not yet submitted; page loads must not overwrite it.
  const dirty = useRef(false);
  useEffect(() => {
    if (dirty.current) return;
    setValue(url);
    // A redirect (e.g. to a sign-in page) must not drop the selection that lets a paste replace the URL.
    const el = inputRef.current;
    if (el && document.activeElement === el) requestAnimationFrame(() => el.select());
  }, [url]);
  // Selected, so a paste replaces the home page URL.
  useEffect(() => {
    if (autoFocus) inputRef.current?.select();
  }, [autoFocus]);
  return (
    <form
      className="address"
      onSubmit={(e) => {
        e.preventDefault();
        dirty.current = false;
        onGo(value);
      }}
    >
      <input
        ref={inputRef}
        value={value}
        onChange={(e) => {
          dirty.current = true;
          setValue(e.target.value);
        }}
        onBlur={() => {
          dirty.current = false;
          setValue(url);
        }}
        placeholder="输入或粘贴网址，按回车打开"
        spellCheck={false}
      />
    </form>
  );
}

function CredentialDialog({
  account,
  site,
  onClose,
  onSaved,
}: {
  account: Account;
  site: Site;
  onClose: () => void;
  onSaved: () => Promise<void>;
}) {
  // The username comes from sites.json, so it shows without a Keychain prompt. The
  // password is read from the Keychain only when needed (显示 / 复制 / 编辑), which asks
  // for the Mac password at most once per account until the app quits.
  const [cred, setCred] = useState<Credential>({ username: account.username ?? "", password: "", note: "" });
  const [unlocked, setUnlocked] = useState(false);
  // The account's home address (kept in sites.json, not the keychain); empty means the site's URL.
  const [address, setAddress] = useState(account.url ?? site.url);
  const [loaded, setLoaded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [reveal, setReveal] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    api
      .credentialStatus(account.id)
      .then(async ({ saved, credential }) => {
        if (credential) {
          setCred(credential);
          setUnlocked(true);
        } else if (!saved) {
          setUnlocked(true);
          setEditing(true);
        } else if (!account.username) {
          // Saved before 0.7.8, which started mirroring the username into sites.json:
          // read the Keychain once so the username shows now and without a prompt from
          // then on (get_credential copies it into sites.json).
          const c = await api.getCredential(account.id).catch((e) => {
            setMessage(`${e}（点「显示」可重试）`);
            return null;
          });
          if (c) {
            setCred(c);
            setUnlocked(true);
            onSaved();
          }
        }
        setLoaded(true);
      })
      .catch((e) => {
        setMessage(String(e));
        setLoaded(true);
      });
  }, [account.id]);

  // Reads the saved login from the Keychain; resolves to null if that failed.
  const unlock = async (): Promise<Credential | null> => {
    if (unlocked) return cred;
    try {
      const c = (await api.getCredential(account.id)) ?? { username: "", password: "", note: "" };
      setCred(c);
      setUnlocked(true);
      setMessage(null);
      return c;
    } catch (e) {
      setMessage(String(e));
      return null;
    }
  };

  const copy = async (key: "username" | "password", label: string) => {
    const text = key === "username" && !unlocked ? cred.username : (await unlock())?.[key];
    if (text === undefined) return;
    try {
      await writeText(text);
      setMessage(`已复制${label}`);
    } catch (e) {
      setMessage(String(e));
    }
  };

  const save = async () => {
    try {
      await api.setCredential(account.id, cred);
      const url = address.trim();
      await api.setAccountUrl(account.id, url === site.url ? "" : url);
      await onSaved();
      onClose();
    } catch (e) {
      setMessage(String(e));
    }
  };

  // Before unlocking, a saved password is shown as dots without its real length.
  const shown = (key: "username" | "password") => (key === "password" && !unlocked ? "••••••••" : cred[key]);

  const field = (key: "username" | "password", label: string, placeholder: string, secret = false) => (
    <label>
      {label}
      <div className="field-row">
        <input
          type={secret && !reveal ? "password" : "text"}
          value={shown(key)}
          readOnly={!editing}
          onChange={(e) => setCred((c) => ({ ...c, [key]: e.target.value }))}
          placeholder={editing ? placeholder : ""}
          aria-label={label}
          spellCheck={false}
          autoComplete="off"
        />
        {secret && (
          <button
            type="button"
            aria-label={reveal ? "隐藏密码" : "显示密码"}
            onClick={async () => {
              if (reveal) setReveal(false);
              else if (await unlock()) setReveal(true);
            }}
          >
            {reveal ? "隐藏" : "显示"}
          </button>
        )}
        {!editing && (
          <button
            type="button"
            aria-label={`复制${label}`}
            disabled={!shown(key)}
            onClick={() => copy(key, label)}
          >
            复制
          </button>
        )}
      </div>
    </label>
  );

  return (
    <div className="backdrop" onClick={onClose}>
      <form
        className="dialog"
        onClick={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          if (editing) save();
        }}
      >
        <h3>「{account.name}」的账号密码</h3>
        {loaded && (
          <>
            {field("username", "账号", "用户名 / 邮箱 / 手机号")}
            {field("password", "密码", "密码", true)}
            <label>
              地址
              <div className="field-row">
                <input
                  value={address}
                  readOnly={!editing}
                  onChange={(e) => setAddress(e.target.value)}
                  placeholder={editing ? `留空则打开 ${site.url}` : ""}
                  aria-label="地址"
                  spellCheck={false}
                  autoComplete="off"
                />
              </div>
            </label>
          </>
        )}
        <p className="hint-text">
          {message ?? "打开这个账号时进入「地址」里的网址。密码只保存在系统钥匙串里，点「显示」时才读取。"}
        </p>
        <div className="dialog-actions">
          {editing ? (
            <>
              <button type="button" onClick={onClose}>
                取消
              </button>
              <button type="submit" className="primary">
                保存
              </button>
            </>
          ) : (
            <>
              <button
                type="button"
                onClick={async () => {
                  if (await unlock()) setEditing(true);
                }}
              >
                编辑
              </button>
              <button type="button" className="primary" onClick={onClose}>
                关闭
              </button>
            </>
          )}
        </div>
      </form>
    </div>
  );
}

function UpdateDialog({
  update,
  error,
  version,
  onClose,
}: {
  update: Update | null;
  error?: string;
  version: string;
  onClose: () => void;
}) {
  // Non-null while downloading / installing; the dialog cannot be dismissed then.
  const [progress, setProgress] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const close = () => {
    if (progress !== null) return;
    update?.close().catch(() => {});
    onClose();
  };

  // Downloads, checks the signature against the public key in tauri.conf.json,
  // replaces the app and restarts it.
  const install = async () => {
    if (!update) return;
    let total = 0;
    let received = 0;
    setMessage(null);
    setProgress("正在下载…");
    try {
      await update.downloadAndInstall((e) => {
        if (e.event === "Started") total = e.data.contentLength ?? 0;
        else if (e.event === "Progress") {
          received += e.data.chunkLength;
          if (total) setProgress(`正在下载… ${Math.min(100, Math.floor((received * 100) / total))}%`);
        } else setProgress("正在安装…");
      });
      setProgress("安装完成，正在重启…");
      await relaunch();
    } catch (e) {
      setProgress(null);
      setMessage(`更新失败：${e}`);
    }
  };

  const title = update ? `发现新版本 ${update.version}` : error ? "检查更新失败" : "已是最新版本";
  return (
    <div className="backdrop" onClick={close}>
      <form
        className="dialog"
        onClick={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          if (update) install();
          else close();
        }}
      >
        <h3>{title}</h3>
        {update ? (
          <>
            <p>当前版本 {update.currentVersion}。更新后 app 会自动重启，网站、账号、登录状态和保存的密码都会保留。</p>
            {update.body && <div className="release-notes">{update.body}</div>}
          </>
        ) : (
          <p>{error ?? `当前版本 ${version} 已是最新。`}</p>
        )}
        {(progress ?? message) && <p className="hint-text">{progress ?? message}</p>}
        <div className="dialog-actions">
          {update ? (
            <>
              <button type="button" onClick={close} disabled={progress !== null}>
                以后再说
              </button>
              <button type="submit" className="primary" disabled={progress !== null}>
                立即更新
              </button>
            </>
          ) : (
            <button type="submit" className="primary">
              好
            </button>
          )}
        </div>
      </form>
    </div>
  );
}

function Dialog({
  state,
  onClose,
  onDone,
  run,
}: {
  state: Exclude<DialogState, { kind: "credential" | "update" }>;
  onClose: () => void;
  onDone: () => Promise<void>;
  run: (fn: () => Promise<unknown>) => Promise<void>;
}) {
  const initial = (() => {
    switch (state.kind) {
      case "editSite":
        return { name: state.site.name, url: state.site.url };
      case "renameAccount":
        return { name: state.account.name, url: "" };
      default:
        return { name: "", url: "" };
    }
  })();
  const [name, setName] = useState(initial.name);
  const [url, setUrl] = useState(initial.url);

  const submit = async () => {
    await run(async () => {
      switch (state.kind) {
        case "addSite":
          await api.addSite(name, url);
          break;
        case "editSite":
          await api.updateSite(state.site.id, name, url);
          break;
        case "addAccount":
          await api.addAccount(state.site.id, name);
          break;
        case "renameAccount":
          await api.renameAccount(state.account.id, name);
          break;
        case "confirm":
          await state.onConfirm();
          break;
      }
      await onDone();
    });
    onClose();
  };

  const title = {
    addSite: "添加网站",
    editSite: "编辑网站",
    addAccount: state.kind === "addAccount" ? `给「${state.site.name}」新增账号` : "",
    renameAccount: "重命名账号",
    confirm: "确认删除",
  }[state.kind];

  return (
    <div className="backdrop" onClick={onClose}>
      <form
        className="dialog"
        onClick={(e) => e.stopPropagation()}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <h3>{title}</h3>
        {state.kind === "confirm" ? (
          <p>{state.message}</p>
        ) : (
          <>
            <label>
              {state.kind === "addAccount" || state.kind === "renameAccount" ? "备注名" : "名称"}
              <input
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder={state.kind === "addAccount" ? "如：工作号（可留空）" : "如：GitHub"}
              />
            </label>
            {(state.kind === "addSite" || state.kind === "editSite") && (
              <label>
                网址
                <input
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                  placeholder="https://github.com"
                  required
                  spellCheck={false}
                />
              </label>
            )}
          </>
        )}
        <div className="dialog-actions">
          <button type="button" onClick={onClose}>
            取消
          </button>
          <button type="submit" className={state.kind === "confirm" ? "primary danger" : "primary"}>
            {state.kind === "confirm" ? "删除" : "确定"}
          </button>
        </div>
      </form>
    </div>
  );
}

export default App;
