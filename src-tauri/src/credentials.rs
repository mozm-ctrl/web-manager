//! Saved login details per account, kept in the OS credential store (macOS
//! Keychain / Windows Credential Manager / Linux Secret Service) — never in
//! sites.json.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const SERVICE: &str = "com.mozm.webmanager";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credential {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub note: String,
}

fn entry(account_id: Uuid) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &account_id.to_string()).map_err(|e| e.to_string())
}

pub fn get(account_id: Uuid) -> Result<Option<Credential>, String> {
    match entry(account_id)?.get_password() {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| e.to_string()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("读取钥匙串失败：{e}")),
    }
}

/// Saves the credential; an all-empty credential removes the entry instead.
pub fn set(account_id: Uuid, cred: &Credential) -> Result<(), String> {
    if cred.username.is_empty() && cred.password.is_empty() && cred.note.is_empty() {
        return delete(account_id);
    }
    let text = serde_json::to_string(cred).map_err(|e| e.to_string())?;
    entry(account_id)?
        .set_password(&text)
        .map_err(|e| format!("写入钥匙串失败：{e}"))
}

pub fn delete(account_id: Uuid) -> Result<(), String> {
    match entry(account_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("删除钥匙串条目失败：{e}")),
    }
}
