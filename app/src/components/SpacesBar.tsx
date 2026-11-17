import { ask, open } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState } from "react";
import { type AppInfo, desktop } from "../lib/backend";
import { PLACES, type Place } from "../lib/places";
import { say } from "../lib/dialog";

/**
 * Kernel #53: the bar across the top is places (Home, Folders, Teams, Activity, Apps), today's spend and the Settings
 * gear. Home counts what needs you.
 */
export function PlacesBar({
  place,
  onPlace,
  needs,
  todayUsd,
}: {
  place: Place;
  onPlace: (place: Place) => void;
  /** How many things need you (Home's count). */
  needs: number;
  /** Today's spend, when known. */
  todayUsd: number | null;
}) {
  return (
    <nav className="spaces" aria-label="Places">
      {PLACES.map((p) => {
        // Teams open from the folder tree, so Folders stays lit there; Apps from Settings (the gear).
        const on = place === p.id || (p.id === "folders" && place === "teams");
        return (
        <button key={p.id} className={`space${on ? " active" : ""}`} aria-current={on ? "page" : undefined} title={p.title} onClick={() => onPlace(p.id)}>
          {p.label}
          {p.id === "home" && needs > 0 && (
            <span className="place-count" aria-label={`${needs} need you`}>
              {needs}
            </span>
          )}
        </button>
        );
      })}
      <span className="grow" />
      {todayUsd !== null && (
        <span className="place-spend" title="Today's spend, everything priced (marv -p and workers too)">
          today ${todayUsd.toFixed(2)}
        </span>
      )}
      <button
        className={`space place-gear${place === "settings" || place === "apps" ? " active" : ""}`}
        aria-label="Settings"
        title="Settings: providers, keys, models, budgets, theme"
        onClick={() => onPlace("settings")}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
        </svg>
      </button>
    </nav>
  );
}

/**
 * The spaces (each a top folder with its own chats and switched-off apps), now a menu at the top of the Folders
 * sidebar: switch, rename (double-click or ✎), remove, or make a new one.
 */
export function SpaceMenu({ info, onInfo }: { info: AppInfo; onInfo: (info: AppInfo) => void }) {
  const [open, setOpen] = useState(false);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [name, setName] = useState("");
  const ref = useRef<HTMLDivElement>(null);
  const active = info.spaces.find((s) => s.id === info.active) ?? info.spaces[0];

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  async function create() {
    const path = await openDialog();
    if (typeof path !== "string") return;
    try {
      onInfo(await desktop.createSpace(path.split(/[\\/]/).filter(Boolean).pop() ?? "space", path));
      setOpen(false);
    } catch (e) {
      alertError(e);
    }
  }

  async function remove(id: string, spaceName: string) {
    if (!(await ask(`Remove the space "${spaceName}"? Its folder, files and chats stay.`, { title: "Marv", kind: "warning" }))) return;
    try {
      onInfo(await desktop.removeSpace(id));
    } catch (e) {
      alertError(e);
    }
  }

  async function rename(id: string) {
    setRenaming(null);
    if (!name.trim()) return;
    try {
      onInfo(await desktop.renameSpace(id, name));
    } catch (e) {
      alertError(e);
    }
  }

  return (
    <div className="space-menu" ref={ref}>
      <button className="space-current" aria-expanded={open} title={`${active?.root ?? ""} · your spaces`} onClick={() => setOpen((o) => !o)}>
        <span className="label">{active?.name ?? info.rootName ?? "Marv"}</span>
        <span className="faint">▾</span>
      </button>
      {open && (
        <div className="space-list" role="menu" aria-label="Spaces">
          {info.spaces.map((space) =>
            renaming === space.id ? (
              <input
                key={space.id}
                className="space-edit"
                autoFocus
                value={name}
                onChange={(e) => setName(e.target.value)}
                onBlur={() => void rename(space.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void rename(space.id);
                  if (e.key === "Escape") setRenaming(null);
                }}
              />
            ) : (
              <div
                key={space.id}
                role="menuitem"
                className={`space-item${space.id === info.active ? " active" : ""}`}
                title={`${space.root} · double-click to rename`}
                onClick={() => {
                  if (space.id !== info.active) void desktop.switchSpace(space.id).then(onInfo, alertError);
                  setOpen(false);
                }}
                onDoubleClick={() => (setRenaming(space.id), setName(space.name))}
              >
                <span className="label">{space.name}</span>
                <span className="actions" onClick={(e) => e.stopPropagation()}>
                  <button className="icon-btn" title="Rename" onClick={() => (setRenaming(space.id), setName(space.name))}>
                    ✎
                  </button>
                  {info.spaces.length > 1 && (
                    <button className="icon-btn danger" title="Remove this space" onClick={() => void remove(space.id, space.name)}>
                      ×
                    </button>
                  )}
                </span>
              </div>
            ),
          )}
          <button className="space-item add" onClick={() => void create()} title="New space: a folder with its own chats and apps">
            + New space…
          </button>
        </div>
      )}
    </div>
  );
}

const openDialog = () => open({ directory: true, title: "The new space's folder" });

function alertError(e: unknown) {
  void say(e, { title: "Marv", kind: "error" });
}
