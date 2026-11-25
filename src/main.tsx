import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { CredentialPeek } from "./CredentialPeek";

// The app's own UI has no use for WebKit's right-click menu (Reload, AutoFill…), so
// turn it off everywhere except text fields, where copy / paste stay available.
// Account pages are separate webviews and keep theirs.
document.addEventListener("contextmenu", (e) => {
  const t = e.target as HTMLElement | null;
  if (!t?.closest("input, textarea, [contenteditable='true']")) e.preventDefault();
});

// The small credential window loads the same page with ?peek=<account id>.
const peekId = new URLSearchParams(location.search).get("peek");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {peekId ? <CredentialPeek accountId={peekId} /> : <App />}
  </React.StrictMode>,
);
