import type { Status } from "../api/types";
import { shown } from "../lib/text";
import { Icon, type IconName } from "./Icon";

const LEVEL_ICON: Record<string, IconName> = { fatal: "fatal", error: "error", warning: "warning", warn: "warning" };

/** A level with its shape, so the level never rests on color alone. */
export function Level({ level }: { level: string }) {
  const icon = LEVEL_ICON[level] ?? "info";
  const tone = icon === "info" ? "info" : icon;
  return (
    <span className={`lv lv-${tone}`}>
      <Icon name={icon} />
      <span>{shown(level)}</span>
    </span>
  );
}

export function StatusMark({ status }: { status: Status }) {
  return (
    <span className={`stt st-${status}`}>
      <Icon name={status} />
      <span>{status}</span>
    </span>
  );
}

export function Regressed() {
  return (
    <span className="reg flush" title="Regressed: was resolved, seen again">
      <Icon name="regress" />
      regressed
    </span>
  );
}

