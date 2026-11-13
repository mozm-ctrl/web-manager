import { invoke } from "@tauri-apps/api/core";

export interface Account {
  id: string;
  name: string;
}

export interface Site {
  id: string;
  name: string;
  url: string;
  accounts: Account[];
}

export interface PageInfo {
  accountId: string;
  url: string;
  title: string | null;
  loading: boolean;
}

export interface Credential {
  username: string;
  password: string;
  note: string;
}

export type NavAction = "back" | "forward" | "reload" | "home" | "go";

export const api = {
  listSites: () => invoke<Site[]>("list_sites"),
  addSite: (name: string, url: string) => invoke<Site>("add_site", { name, url }),
  updateSite: (id: string, name: string, url: string) =>
    invoke<void>("update_site", { id, name, url }),
  deleteSite: (id: string) => invoke<void>("delete_site", { id }),
  addAccount: (siteId: string, name: string) =>
    invoke<Account>("add_account", { siteId, name }),
  renameAccount: (id: string, name: string) => invoke<void>("rename_account", { id, name }),
  deleteAccount: (id: string) => invoke<void>("delete_account", { id }),
  openAccount: (id: string) => invoke<void>("open_account", { id }),
  closeActive: () => invoke<void>("close_active"),
  setContentBounds: (bounds: {
    x: number;
    y: number;
    width: number;
    height: number;
    viewportHeight: number;
  }) =>
    invoke<void>("set_content_bounds", { bounds }),
  setOverlay: (visible: boolean) => invoke<void>("set_overlay", { visible }),
  navigate: (id: string, action: NavAction, url?: string) =>
    invoke<void>("navigate_account", { id, action, url }),
  getCredential: (id: string) => invoke<Credential | null>("get_credential", { id }),
  setCredential: (id: string, credential: Credential) =>
    invoke<void>("set_credential", { id, credential }),
};
