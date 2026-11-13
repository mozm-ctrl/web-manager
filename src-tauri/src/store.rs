use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One login identity on a site. Only the id (which keys the isolated browser
/// data store) and a display name are kept — never credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub accounts: Vec<Account>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub sites: Vec<Site>,
    /// Accounts whose browser data could not be removed yet; retried on start.
    #[serde(default, rename = "pendingPurge")]
    pub pending_purge: Vec<Uuid>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("failed to parse {}: {e}; starting empty", path.display());
                let backup = path.with_extension("json.broken");
                let _ = std::fs::copy(path, backup);
                Store::default()
            }),
            Err(_) => Store::default(),
        }
    }

    /// Writes atomically (temp file + rename) so a crash never leaves a half-written file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn site_mut(&mut self, id: Uuid) -> Option<&mut Site> {
        self.sites.iter_mut().find(|s| s.id == id)
    }

    pub fn account_mut(&mut self, id: Uuid) -> Option<&mut Account> {
        self.sites
            .iter_mut()
            .flat_map(|s| s.accounts.iter_mut())
            .find(|a| a.id == id)
    }
}
