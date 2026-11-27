import { useId } from "react";
import { LampParts } from "./Icon";

/** A lit window drawn as a brass engraving, with Davy's lamp hanging in it.
    The login, welcome and setup screens carry it; the working pages do not. */
export function LitWindow({ height = 90 }: { height?: number }) {
  const glass = useId();
  return (
    <svg className="window" width={Math.round((height * 72) / 90)} height={height} viewBox="0 0 72 90" aria-hidden="true" focusable="false">
      <defs>
        <linearGradient id={glass} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="var(--glass-2)" />
          <stop offset="0.36" stopColor="var(--glass-3)" />
          <stop offset="0.7" stopColor="var(--glass-2)" />
          <stop offset="1" stopColor="var(--glass-1)" />
        </linearGradient>
      </defs>
      <path className="wn-line" d="M24 9.5 36 3l12 6.5" />
      <path className="wn-line" d="M5 9.5h62" />
      <rect x="12.5" y="15.5" width="47" height="67" fill={`url(#${glass})`} />
      <path className="wn-bar" d="M12.5 62h47M36 62v20.5" />
      <rect className="wn-line" x="9" y="12" width="54" height="74" />
      <path className="wn-wire" d="M36 15.5v6" />
      <g className="lamp in" transform="translate(19.2 19.6) scale(2.1)">
        <LampParts />
      </g>
      <rect className="wn-sill" x="5" y="86" width="62" height="4" />
    </svg>
  );
}
