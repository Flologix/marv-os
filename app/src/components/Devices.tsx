import { useEffect, useState } from "react";
import { desktop, isDesktop } from "../lib/backend";

const DEVICE_LABELS: Record<string, string> = {
  github: "GitHub (gh)",
  herdr: "Herdr panes",
  process: "Dev servers",
  host: "Commands on this machine",
};

export interface ChatDevices {
  names: string[];
  off: string[];
  onToggle: (name: string) => void;
}

/**
 * A chat's device switches (runtime-kernel §17.1): saved with it, used from its next start (its device set is fixed
 * for a boot). The list is the chat's own (its folder's servers and yours, running or not), built by the desktop
 * (`chat_devices`). Undefined for a draft or in a browser.
 */
export function useChatDevices(chatId: string | null | undefined): ChatDevices | undefined {
  const [off, setOff] = useState<string[] | null>(null);
  const [names, setNames] = useState<string[]>([]);
  useEffect(() => {
    if (!isDesktop || !chatId) return setOff(null);
    desktop.chatDevices(chatId).then(
      (d) => {
        setNames(d.names);
        setOff(d.off);
      },
      () => setOff(null),
    );
  }, [chatId]);
  if (!isDesktop || !chatId || !off) return undefined;
  return {
    names,
    off,
    onToggle: (name: string) => {
      const next = off.includes(name) ? off.filter((d) => d !== name) : [...off, name];
      void desktop.setChatDevices(chatId, next).then(setOff);
    },
  };
}

/** The switches: in the chat's settings popover and in Apps (for the chat open in Folders). */
export function DeviceSwitches({ devices }: { devices: ChatDevices }) {
  return (
    <>
      {devices.names.map((name) => {
        const on = !devices.off.includes(name);
        return (
          <div className="switch" key={name}>
            <button type="button" role="switch" aria-checked={on} aria-label={name} className={`toggle${on ? " on" : ""}`} onClick={() => devices.onToggle(name)} />
            <div>{DEVICE_LABELS[name] ?? name.replace(/^mcp:/, "")}</div>
          </div>
        );
      })}
      <span className="help">Saved for this chat: applies when it next starts.</span>
    </>
  );
}
