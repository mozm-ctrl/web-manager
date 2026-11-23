import { useEffect, useState } from "react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { api, type Credential } from "./api";
import "./App.css";

/**
 * The small always-on-top window opened by the 👁 button: the account's saved login
 * with a copy button for each field. It can sit next to the page's login form while
 * the user pastes into it.
 */
export function CredentialPeek({ accountId }: { accountId: string }) {
  const [cred, setCred] = useState<Credential | null>(null);
  const [reveal, setReveal] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  // Same origin as the main window, so the theme chosen there applies here too.
  useEffect(() => {
    const apply = () => {
      document.documentElement.dataset.theme = localStorage.getItem("theme") === "dark" ? "dark" : "light";
    };
    apply();
    window.addEventListener("storage", apply);
    return () => window.removeEventListener("storage", apply);
  }, []);

  // May ask for the Mac password, at most once per account until the app quits.
  const load = () => {
    setFailed(false);
    setMessage(null);
    api
      .getCredential(accountId)
      .then((c) => setCred(c ?? { username: "", password: "", note: "" }))
      .catch((e) => {
        setFailed(true);
        setMessage(String(e));
      });
  };
  useEffect(load, [accountId]);

  const copy = async (text: string, label: string) => {
    try {
      await writeText(text);
      setMessage(`已复制${label}`);
    } catch (e) {
      setMessage(String(e));
    }
  };

  const empty = cred !== null && !cred.username && !cred.password;
  return (
    <div className="peek">
      {cred === null ? (
        <p className="peek-hint">{failed ? message : "正在读取…"}</p>
      ) : empty ? (
        <p className="peek-hint">这个账号还没有保存账号密码，请在侧栏点 🔑 填写。</p>
      ) : (
        <>
          <div className="peek-row">
            <span className="peek-label">账号</span>
            <span className="peek-value" title={cred.username}>
              {cred.username || "（空）"}
            </span>
            <button type="button" disabled={!cred.username} onClick={() => copy(cred.username, "账号")}>
              复制
            </button>
          </div>
          <div className="peek-row">
            <span className="peek-label">密码</span>
            <span className="peek-value">{cred.password ? (reveal ? cred.password : "••••••••") : "（空）"}</span>
            {cred.password && (
              <button
                type="button"
                className="icon"
                title={reveal ? "隐藏密码" : "显示密码"}
                aria-label={reveal ? "隐藏密码" : "显示密码"}
                onClick={() => setReveal((r) => !r)}
              >
                {reveal ? "隐藏" : "显示"}
              </button>
            )}
            <button type="button" disabled={!cred.password} onClick={() => copy(cred.password, "密码")}>
              复制
            </button>
          </div>
          {message && <p className="peek-hint">{message}</p>}
        </>
      )}
      {failed && (
        <button type="button" onClick={load}>
          重试
        </button>
      )}
    </div>
  );
}
