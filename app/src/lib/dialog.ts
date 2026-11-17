import { message } from "@tauri-apps/plugin-dialog";
import { shouldSay } from "./errors";

/** Kernel #41: set as the page goes (a reload, the watchdog's included): nothing opens a dialog from then on. */
let unloading = false;
if (typeof window !== "undefined") {
  const going = () => {
    unloading = true;
  };
  window.addEventListener("pagehide", going);
  window.addEventListener("beforeunload", going);
}

/**
 * The person is told `what` in a native dialog, unless the page is unloading or the error is the link to the app
 * breaking (`errors.ts`): then it's logged. A dialog that can't open (no native dialogs here) is logged too.
 */
export function say(what: unknown, options: { title?: string; kind?: "info" | "warning" | "error" } = { title: "Marv" }): Promise<void> {
  const text = String(what);
  if (!shouldSay(what, unloading)) {
    console.warn(`marv: ${text}`);
    return Promise.resolve();
  }
  return message(text, options).then(
    () => {},
    (e) => console.warn(`marv: ${text} (no dialog: ${String(e)})`),
  );
}
