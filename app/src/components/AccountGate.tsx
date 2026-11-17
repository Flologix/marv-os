import type { ReactNode } from "react";
import { type AppInfo, desktop } from "../lib/backend";
import { Martian } from "./Martian";
import { MarvPlanSignIn } from "./MarvPlan";

/**
 * A feature that needs a (free) Marv account: Teams, connecting apps. Signed in, its page as usual; signed out, one card
 * to sign up in place (Google, GitHub or an emailed code), and the page once that's done. The backend checks the same
 * (`needs_account` in lib.rs), so the card is the way in, not the lock.
 */
export function AccountGate({
  info,
  onInfo,
  feature,
  pitch,
  children,
}: {
  info: AppInfo;
  onInfo: (info: AppInfo) => void;
  /** "Teams", "Apps". */
  feature: string;
  /** One line on what it gives. */
  pitch: string;
  children: ReactNode;
}) {
  if (info.signedIn) return <>{children}</>;
  return (
    <div className="account-gate">
      <Martian size={48} className="toon" />
      <h2>Sign up free to use {feature}</h2>
      <p className="muted">{pitch}</p>
      <MarvPlanSignIn onSignedIn={() => void desktop.info().then(onInfo)} />
      <p className="faint small">A free account: no card, nothing to pay. Everything else in Marv OS works without one.</p>
    </div>
  );
}
