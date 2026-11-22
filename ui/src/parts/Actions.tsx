import type { Status } from "../api/types";
import { Icon } from "./Icon";

const ACTIONS: [Status, string, string][] = [
  ["resolved", "Resolve", "r"],
  ["ignored", "Ignore", "i"],
  ["unresolved", "Reopen", "o"],
];

/** Resolve, Ignore and Reopen. The one matching `status` is off. */
export function StatusButtons({ status, busy, keys, onAct }: { status: Status | null; busy: boolean; keys: boolean; onAct: (s: Status) => void }) {
  return ACTIONS.map(([to, label, key]) => (
    <button key={to} type="button" className="btn" disabled={busy || status === to} onClick={() => onAct(to)}>
      <Icon name={to} />
      {label}
      {keys ? <kbd>{key}</kbd> : null}
    </button>
  ));
}

/** The status a key asks for, if it is one of r, i and o. */
export function statusKey(key: string): Status | undefined {
  return ACTIONS.find(([, , k]) => k === key)?.[0];
}
