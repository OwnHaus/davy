import { useEffect, useState } from "react";


// One polite live region for the whole app. Clearing it first makes a screen
// reader announce the same words twice in a row.

let region: HTMLElement | null = null;

export function liveRegion(el: HTMLElement | null) {
  region = el;
}

export function announce(words: string) {
  const el = region;
  if (!el) return;
  el.textContent = "";
  window.setTimeout(() => {
    el.textContent = words;
  }, 30);
}

export async function copy(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

export const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/** `value`, once it has held still for `ms`, so a held j key fetches nothing. */
export function useSettled<T>(value: T, ms: number): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const t = window.setTimeout(() => setSettled(value), ms);
    return () => window.clearTimeout(t);
  }, [value, ms]);
  return settled;
}

/** The time now, renewed every minute, for the `5m` ages. */
export function useNow(): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(t);
  }, []);
  return now;
}
