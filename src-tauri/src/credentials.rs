//! Saved login details, kept in the OS credential store (macOS Keychain / Windows
//! Credential Manager / Linux Secret Service) — never in sites.json.
//!
//! Since 0.9.0 all accounts share one item (the "vault"), so macOS asks for the Mac
//! password once per launch instead of once per account. Items of older versions
//! (one per account, keyed by account id) are moved into the vault when it is first
//! unlocked; see `unlock_vault` in lib.rs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const SERVICE: &str = "com.mozm.webmanager";
/// Keychain account name of the item holding every saved login.
const VAULT: &str = "vault";

/// Saved logins by account id.
pub type Vault = HashMap<Uuid, Credential>;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Credential {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub note: String,
}

impl Credential {
    pub fn is_empty(&self) -> bool {
        self.username.is_empty() && self.password.is_empty() && self.note.is_empty()
    }
}

fn entry(account_id: Uuid) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &account_id.to_string()).map_err(|e| e.to_string())
}

fn vault_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, VAULT).map_err(|e| e.to_string())
}

/// Reads every saved login at once. This is the one read that may ask for the Mac
/// password.
pub fn load_vault() -> Result<Vault, String> {
    match vault_entry()?.get_password() {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("钥匙串里的账号密码无法读取：{e}")),
        Err(keyring::Error::NoEntry) => Ok(Vault::new()),
        Err(e) => Err(format!("读取钥匙串失败：{e}")),
    }
}

/// Replaces the stored vault; an empty one removes the item.
pub fn save_vault(vault: &Vault) -> Result<(), String> {
    if vault.is_empty() {
        return match vault_entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("写入钥匙串失败：{e}")),
        };
    }
    let text = serde_json::to_string(vault).map_err(|e| e.to_string())?;
    vault_entry()?
        .set_password(&text)
        .map_err(|e| format!("写入钥匙串失败：{e}"))
}

/// Reads an account's item from versions before 0.9.0.
pub fn get(account_id: Uuid) -> Result<Option<Credential>, String> {
    match entry(account_id)?.get_password() {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| e.to_string()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("读取钥匙串失败：{e}")),
    }
}

/// Whether an account still has an item from before 0.9.0, read from the item's attributes only. Unlike
/// `get`, this does not touch the secret, so macOS does not ask for the password.
#[cfg(target_os = "macos")]
pub fn exists(account_id: Uuid) -> Result<bool, String> {
    use security_framework::item::{ItemClass, ItemSearchOptions};
    let found = ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(SERVICE)
        .account(&account_id.to_string())
        .load_attributes(true)
        .search();
    match found {
        Ok(items) => Ok(!items.is_empty()),
        // errSecItemNotFound
        Err(e) if e.code() == -25300 => Ok(false),
        Err(e) => Err(format!("读取钥匙串失败：{e}")),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn exists(account_id: Uuid) -> Result<bool, String> {
    get(account_id).map(|c| c.is_some())
}

/// Removes an account's item from before 0.9.0, if any.
pub fn delete(account_id: Uuid) -> Result<(), String> {
    match entry(account_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("删除钥匙串条目失败：{e}")),
    }
}
