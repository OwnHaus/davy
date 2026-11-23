import type { Delivery, State, Status } from "../api/types";
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


const STATE_ICON: Record<State, IconName> = { up: "up", failing: "warning", down: "error", pending: "pending" };

/** A monitor's state with its shape, so down never rests on color alone. */
export function StateMark({ state }: { state: State }) {
  return (
    <span className={`stt ms-${state}`}>
      <Icon name={STATE_ICON[state]} />
      <span>{state}</span>
    </span>
  );
}

const DELIVERY_ICON: Record<Delivery, IconName> = { sent: "check", waiting: "pending", failed: "error" };

export function DeliveryMark({ delivery }: { delivery: Delivery }) {
  return (
    <span className={`stt dl-${delivery}`}>
      <Icon name={DELIVERY_ICON[delivery]} />
      <span>{delivery}</span>
    </span>
  );
}

/** Whether one check passed. */
export function CheckMark({ ok }: { ok: boolean }) {
  return (
    <span className={`stt ${ok ? "ck-ok" : "ck-failed"}`}>
      <Icon name={ok ? "check" : "error"} />
      <span>{ok ? "passed" : "failed"}</span>
    </span>
  );
}
