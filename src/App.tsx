import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { api, type Account, type Credential, type PageInfo, type Site, type TabsInfo } from "./api";
import "./App.css";

type DialogState =
  | { kind: "addSite" }
  | { kind: "editSite"; site: Site }
  | { kind: "addAccount"; site: Site }
  | { kind: "renameAccount"; account: Account }
  | { kind: "credential"; account: Account }
  | { kind: "confirm"; message: string; onConfirm: () => Promise<void> };

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
  const contentRef = useRef<HTMLDivElement>(null);

  const refresh = useCallback(async () => setSites(await api.listSites()), []);

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
    const unlisten = listen<PageInfo>("account-page", ({ payload }) => {
      setPages((p) => ({
        ...p,
        [payload.tabId]: {
          ...payload,
          title: payload.title ?? p[payload.tabId]?.title ?? null,
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

  // Native webviews always sit above the HTML UI, so hide them while a dialog is open.
  useEffect(() => {
    api.setOverlay(dialog !== null);
  }, [dialog]);

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
  const nav = (action: "back" | "forward" | "reload" | "home") =>
    activeTab && run(() => api.navigate(activeTab, action));

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="sidebar-header">
          <span>网站</span>
          <button className="icon" title="添加网站" aria-label="添加网站" onClick={() => setDialog({ kind: "addSite" })}>
            ＋
          </button>
        </div>
        <div className="site-list">
          {sites.length === 0 && <div className="empty">点击右上角 ＋ 添加第一个网站</div>}
          {sites.map((site) => (
            <div key={site.id} className="site">
              <div className="site-row">
                <button
                  className="caret"
                  onClick={() => setCollapsed((c) => ({ ...c, [site.id]: !c[site.id] }))}
                >
                  {collapsed[site.id] ? "▸" : "▾"}
                </button>
                <span className="site-name" title={site.url}>
                  {site.name}
                </span>
                <span className="row-actions">
                  <button className="icon" title="新增账号" aria-label={`给 ${site.name} 新增账号`} onClick={() => setDialog({ kind: "addAccount", site })}>
                    ＋
                  </button>
                  <button className="icon" title="编辑网站" aria-label={`编辑网站 ${site.name}`} onClick={() => setDialog({ kind: "editSite", site })}>
                    ✎
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
                    ✕
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
                          onClick={() => setDialog({ kind: "credential", account: acc })}
                        >
                          🔑
                        </button>
                        <button className="icon" title="重命名" aria-label={`重命名 ${acc.name}`} onClick={() => setDialog({ kind: "renameAccount", account: acc })}>
                          ✎
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
                          ✕
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
      </aside>

      <main className="main">
        <div className="toolbar">
          {active ? (
            <>
              <button className="icon" title="后退" aria-label="后退" onClick={() => nav("back")}>
                ←
              </button>
              <button className="icon" title="前进" aria-label="前进" onClick={() => nav("forward")}>
                →
              </button>
              <button className="icon" title="刷新" aria-label="刷新" onClick={() => nav("reload")}>
                ⟳
              </button>
              <button className="icon" title="回到首页" aria-label="回到首页" onClick={() => nav("home")}>
                ⌂
              </button>
              <button
                className="icon"
                title="查看此账号保存的账号密码"
                aria-label="查看账号密码"
                onClick={() => setDialog({ kind: "credential", account: active.account })}
              >
                🔑
              </button>
              <AddressBar
                key={activeTab ?? active.account.id}
                url={page?.url ?? active.site.url}
                onGo={(url) => activeTab && run(() => api.navigate(activeTab, "go", url))}
              />
              <span className="badge" title={page?.title ?? ""}>
                {active.site.name} · {active.account.name}
                {page?.loading ? " · 加载中" : ""}
              </span>
            </>
          ) : (
            <span className="hint">选择左侧的账号打开网站</span>
          )}
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
                    {p?.loading ? "⋯ " : ""}
                    {label}
                  </button>
                  <button
                    className="icon tab-close"
                    title="关闭标签页"
                    aria-label={`关闭标签 ${label}`}
                    onClick={() => run(() => api.closeTab(tabId))}
                  >
                    ×
                  </button>
                </div>
              );
            })}
            <button
              className="icon"
              title="新建标签页"
              aria-label="新建标签页"
              onClick={() => run(() => api.newTab(active.account.id))}
            >
              ＋
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

      {dialog?.kind === "credential" ? (
        <CredentialDialog account={dialog.account} onClose={() => setDialog(null)} />
      ) : (
        dialog && <Dialog state={dialog} onClose={() => setDialog(null)} onDone={refresh} run={run} />
      )}
    </div>
  );
}

function hostOf(url: string) {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function AddressBar({ url, onGo }: { url: string; onGo: (url: string) => void }) {
  const [value, setValue] = useState(url);
  useEffect(() => setValue(url), [url]);
  return (
    <form
      className="address"
      onSubmit={(e) => {
        e.preventDefault();
        onGo(value);
      }}
    >
      <input value={value} onChange={(e) => setValue(e.target.value)} spellCheck={false} />
    </form>
  );
}

function CredentialDialog({ account, onClose }: { account: Account; onClose: () => void }) {
  const [cred, setCred] = useState<Credential>({ username: "", password: "", note: "" });
  const [loaded, setLoaded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [reveal, setReveal] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    api
      .getCredential(account.id)
      .then((c) => {
        if (c) setCred(c);
        else setEditing(true);
        setLoaded(true);
      })
      .catch((e) => {
        setMessage(String(e));
        setLoaded(true);
      });
  }, [account.id]);

  const copy = async (label: string, text: string) => {
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
      setEditing(false);
      setReveal(false);
      setMessage("已保存到系统钥匙串");
    } catch (e) {
      setMessage(String(e));
    }
  };

  const field = (key: keyof Credential, label: string, placeholder: string, secret = false) => (
    <label>
      {label}
      <div className="field-row">
        <input
          type={secret && !reveal ? "password" : "text"}
          value={cred[key]}
          readOnly={!editing}
          onChange={(e) => setCred((c) => ({ ...c, [key]: e.target.value }))}
          placeholder={editing ? placeholder : ""}
          aria-label={label}
          spellCheck={false}
          autoComplete="off"
        />
        {secret && (
          <button type="button" aria-label={reveal ? "隐藏密码" : "显示密码"} onClick={() => setReveal((r) => !r)}>
            {reveal ? "隐藏" : "显示"}
          </button>
        )}
        {!editing && key !== "note" && (
          <button
            type="button"
            aria-label={`复制${label}`}
            disabled={!cred[key]}
            onClick={() => copy(label, cred[key])}
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
            {field("note", "备注", "可选，如密保问题提示")}
          </>
        )}
        <p className="hint-text">{message ?? "保存在系统钥匙串里，不会写进 sites.json。"}</p>
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
              <button type="button" onClick={() => setEditing(true)}>
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

function Dialog({
  state,
  onClose,
  onDone,
  run,
}: {
  state: Exclude<DialogState, { kind: "credential" }>;
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
