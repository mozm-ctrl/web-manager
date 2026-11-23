import { invoke } from "@tauri-apps/api/core";

export interface Account {
  id: string;
  name: string;
  /** Home address; the site's URL is used when absent. */
  url?: string;
  /** Username of the saved login, readable without a Keychain prompt. */
  username?: string;
}

export interface CredentialStatus {
  saved: boolean;
  /** Present once the login was read this session. */
  credential: Credential | null;
}

export interface Site {
  id: string;
  name: string;
  url: string;
  accounts: Account[];
}

export interface PageInfo {
  accountId: string;
  tabId: string;
  url: string;
  title: string | null;
  loading: boolean;
}

/** As emitted by the backend: `loading` is absent when the event does not change it. */
export interface PageEvent extends Omit<PageInfo, "loading"> {
  loading: boolean | null;
}

export interface TabsInfo {
  accountId: string;
  tabs: string[];
  activeTab: string | null;
}

export interface Credential {
  username: string;
  password: string;
  note: string;
}

/** Payload of the `download` event; one id per download, reported again as it changes. */
export interface DownloadInfo {
  id: string;
  accountId: string;
  url: string;
  fileName: string;
  /** Absolute path in the Downloads folder; empty if the request was never seen. */
  path: string;
  state: "started" | "done" | "failed";
}

export type NavAction = "back" | "forward" | "reload" | "stop" | "home" | "go";

export const api = {
  listSites: () => invoke<Site[]>("list_sites"),
  addSite: (name: string, url: string) => invoke<Site>("add_site", { name, url }),
  updateSite: (id: string, name: string, url: string) =>
    invoke<void>("update_site", { id, name, url }),
  deleteSite: (id: string) => invoke<void>("delete_site", { id }),
  addAccount: (siteId: string, name: string) =>
    invoke<Account>("add_account", { siteId, name }),
  renameAccount: (id: string, name: string) => invoke<void>("rename_account", { id, name }),
  setAccountUrl: (id: string, url: string) => invoke<void>("set_account_url", { id, url }),
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
  newTab: (accountId: string) => invoke<string>("new_tab", { accountId }),
  selectTab: (tabId: string) => invoke<void>("select_tab", { tabId }),
  closeTab: (tabId: string) => invoke<void>("close_tab", { tabId }),
  navigate: (tabId: string, action: NavAction, url?: string) =>
    invoke<void>("navigate_tab", { tabId, action, url }),
  credentialStatus: (id: string) => invoke<CredentialStatus>("credential_status", { id }),
  getCredential: (id: string) => invoke<Credential | null>("get_credential", { id }),
  setCredential: (id: string, credential: Credential) =>
    invoke<void>("set_credential", { id, credential }),
  openCredentialPeek: (id: string) => invoke<void>("open_credential_peek", { id }),
  openDownload: (path: string) => invoke<void>("open_download", { path }),
  revealDownload: (path: string) => invoke<void>("reveal_download", { path }),
};
