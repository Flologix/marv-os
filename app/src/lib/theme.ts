// The desktop's theme (kernel #53, Settings): Flexoki light or dark, or the system's. A per-window convenience, kept in
// this window's storage; styles.css has both palettes (`:root` and `:root[data-theme="dark"]`).
export type ThemePref = "system" | "light" | "dark";

const KEY = "marv-theme";

export function storedTheme(): ThemePref {
  try {
    const value = localStorage.getItem(KEY);
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
}

let stop: (() => void) | null = null;

/** Applies `pref` now (and, for "system", whenever the system's changes) and remembers it. */
export function applyTheme(pref: ThemePref) {
  try {
    localStorage.setItem(KEY, pref);
  } catch {}
  stop?.();
  stop = null;
  const set = (dark: boolean) => {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
  };
  if (pref !== "system") return set(pref === "dark");
  const query = window.matchMedia?.("(prefers-color-scheme: dark)");
  set(query?.matches ?? false);
  if (!query) return;
  const change = (e: MediaQueryListEvent) => set(e.matches);
  query.addEventListener("change", change);
  stop = () => query.removeEventListener("change", change);
}
