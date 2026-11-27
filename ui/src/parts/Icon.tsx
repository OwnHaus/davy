// Drawn for Davy on a 14 grid. The vertical slit in the status marks is the
// house motif.

import type { ReactNode } from "react";

const line = { fill: "none", stroke: "currentColor", strokeLinecap: "round", strokeLinejoin: "round" } as const;
const box = <rect x="1.8" y="1.8" width="10.4" height="10.4" rx="2.4" fill="none" stroke="currentColor" strokeWidth="1.6" />;

const PATHS = {
  fatal: (
    <path fill="currentColor" fillRule="evenodd" d="M7 .7 13.3 7 7 13.3.7 7ZM7 3.6a.9.9 0 0 0-.9.9v3.3a.9.9 0 0 0 1.8 0V4.5A.9.9 0 0 0 7 3.6ZM7 9.4a1 1 0 1 0 0 2 1 1 0 0 0 0-2Z" />
  ),
  error: (
    <path fill="currentColor" fillRule="evenodd" d="M.9 7a6.1 6.1 0 1 0 12.2 0A6.1 6.1 0 0 0 .9 7ZM7 3.4a.9.9 0 0 0-.9.9v3.2a.9.9 0 0 0 1.8 0V4.3A.9.9 0 0 0 7 3.4ZM7 9.3a1 1 0 1 0 0 2 1 1 0 0 0 0-2Z" />
  ),
  warning: (
    <path fill="currentColor" fillRule="evenodd" d="M7 1.1 13.4 12.4H.6ZM7 5.2a.9.9 0 0 0-.9.9v2.5a.9.9 0 0 0 1.8 0V6.1A.9.9 0 0 0 7 5.2ZM7 9.8a1 1 0 1 0 0 1.9 1 1 0 0 0 0-1.9Z" />
  ),
  info: (
    <>
      <circle cx="7" cy="7" r="5.5" fill="none" stroke="currentColor" strokeWidth="1.6" />
      <path d="M7 6.2v3.4" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      <circle cx="7" cy="4.3" r=".95" fill="currentColor" />
    </>
  ),
  unresolved: (
    <>
      {box}
      <path d="M4.2 3.6h2.8v6.8H4.2a.9.9 0 0 1-.9-.9V4.5a.9.9 0 0 1 .9-.9Z" fill="currentColor" />
    </>
  ),
  resolved: (
    <>
      {box}
      <path d="m4.6 7.2 1.7 1.7 3.2-3.6" {...line} strokeWidth="1.7" />
    </>
  ),
  ignored: (
    <>
      {box}
      <path d="M4.6 7h4.8" {...line} strokeWidth="1.7" />
    </>
  ),
  chev: <path d="M4.6 2.9 10 7l-5.4 4.1Z" fill="currentColor" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" />,
  back: <path d="M9 2.9 3.8 7 9 11.1" {...line} strokeWidth="1.7" />,
  fwd: <path d="M5 2.9 10.2 7 5 11.1" {...line} strokeWidth="1.7" />,
  copy: (
    <>
      <rect x="4.7" y="4.7" width="7.4" height="7.4" rx="1.7" fill="none" stroke="currentColor" strokeWidth="1.5" />
      <path d="M9.3 2.3H3.9a1.6 1.6 0 0 0-1.6 1.6v5.4" {...line} strokeWidth="1.5" />
    </>
  ),
  check: <path d="m2.9 7.4 2.9 2.9 5.4-6" {...line} strokeWidth="1.8" />,
  agent: (
    <>
      <path d="M1.6 3.4v7.2M1.6 7h6.8" {...line} strokeWidth="1.7" />
      <circle cx="11.1" cy="7" r="2.4" fill="currentColor" />
    </>
  ),
  smap: <path d="M2.6 2.4v5.3a2 2 0 0 0 2 2h6.4M8.7 6.9l2.6 2.8-2.6 2.8" {...line} strokeWidth="1.6" />,
  regress: <path d="m1.8 10.4 3.8-3.8 2.2 2.2 4.4-4.5M8.8 4.3h3.4v3.4" {...line} strokeWidth="1.7" />,
  sortd: <path d="M7 2.6v8M3.8 8 7 11.2 10.2 8" {...line} strokeWidth="1.7" />,
  filters: <path d="M2 3.5h10M4 7h6M6 10.5h2" {...line} strokeWidth="1.7" />,
  up: (
    <>
      <circle cx="7" cy="7" r="5.5" fill="none" stroke="currentColor" strokeWidth="1.6" />
      <path d="m4.6 7.2 1.7 1.7 3.2-3.6" {...line} strokeWidth="1.7" />
    </>
  ),
  pending: (
    <>
      <circle cx="7" cy="7" r="5.5" fill="none" stroke="currentColor" strokeWidth="1.6" />
      <path d="M7 4.3V7l1.9 1.3" {...line} strokeWidth="1.6" />
    </>
  ),
} satisfies Record<string, ReactNode>;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 13 }: { name: IconName; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 14 14" aria-hidden="true" focusable="false">
      {PATHS[name]}
    </svg>
  );
}

/** The parts of Davy's mark, a brass safety lamp with its flame lit, on a
    16 grid. The classes color it, so it follows the theme. */
export function LampParts() {
  return (
    <>
      <circle className="lm-ring" cx="8" cy="2" r="1.05" />
      <path className="lm" d="M5.4 4.6 8 3.1l2.6 1.5z" />
      <rect className="lm-hi" x="4.9" y="4.5" width="6.2" height="1.05" rx="0.4" />
      <rect className="lm-glass" x="5.5" y="5.55" width="5" height="5.6" />
      <path className="lm-flame" d="M8 6.5c.75 1 1.25 1.65 1.25 2.5a1.25 1.25 0 0 1-2.5 0c0-.85.5-1.5 1.25-2.5z" />
      <path className="lm-core" d="M8 8.3c.3.4.5.65.5 1a.5.5 0 0 1-1 0c0-.35.2-.6.5-1z" />
      <rect className="lm" x="4.7" y="11.15" width="6.6" height="2.9" rx="0.6" />
      <rect className="lm-lo" x="4.7" y="12.3" width="6.6" height="0.5" />
    </>
  );
}

/** Davy's mark, cropped to the lamp so it stands as tall as the name. */
export function Brand({ size = 22 }: { size?: number }) {
  return (
    <svg className="lamp" width={Math.round((size * 9) / 14)} height={size} viewBox="3.5 0.5 9 14" aria-hidden="true" focusable="false">
      <LampParts />
    </svg>
  );
}
