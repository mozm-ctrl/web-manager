import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { CredentialPeek } from "./CredentialPeek";

// The small credential window loads the same page with ?peek=<account id>.
const peekId = new URLSearchParams(location.search).get("peek");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {peekId ? <CredentialPeek accountId={peekId} /> : <App />}
  </React.StrictMode>,
);
