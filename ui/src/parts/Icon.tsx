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
} satisfies Record<string, ReactNode>;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 13 }: { name: IconName; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 14 14" aria-hidden="true" focusable="false">
      {PATHS[name]}
    </svg>
  );
}

export function Brand() {
  return (
    <svg width="20" height="20" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <g fill="currentColor">
        <rect x="1.5" y="1" width="7" height="2.1" rx="1.05" />
        <rect x="1.5" y="4.4" width="10.5" height="2.1" rx="1.05" />
        <rect x="1.5" y="7.8" width="13" height="2.1" rx="1.05" className="accent-fill" />
        <rect x="1.5" y="11.2" width="10.5" height="2.1" rx="1.05" />
      </g>
    </svg>
  );
}
