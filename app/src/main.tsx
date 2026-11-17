import "@fontsource/jost/500.css";
import "@fontsource/jost/600.css";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { logLine } from "./lib/backend";
import "./styles.css";

// The page's own errors go to the app's log too: an exception or a promise nobody caught, with where it came from.
window.addEventListener("error", (e) => logLine("error", "uncaught", `${e.message} (${e.filename}:${e.lineno}:${e.colno})${e.error?.stack ? ` ${e.error.stack}` : ""}`));
window.addEventListener("unhandledrejection", (e) => logLine("error", "unhandled", String(e.reason?.stack ?? e.reason)));

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
