import { getRouteApi } from "@tanstack/react-router";
import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { Refused, get, post } from "../api/client";
import type { LoggedIn, Login as LoginBody, Me } from "../api/types";
import { plural } from "../lib/text";
import { Brand } from "../parts/Icon";

const route = getRouteApi("/login");

// Safari keeps no Secure cookie on http://localhost, so the login succeeds and
// the next request carries no session. Asking who is logged in tells.
export const NO_COOKIE =
  "Davy logged you in, but this browser did not keep the session cookie. The cookie is Secure, and Safari keeps a Secure cookie only over HTTPS. Open Davy over HTTPS, or use another browser on localhost.";

/** After a login or welcome: go on when the cookie stuck, else say why not. */
export async function enter(next: string): Promise<string | null> {
  try {
    await get<Me>("/me");
  } catch (e) {
    if (e instanceof Refused && e.status === 401) return NO_COOKIE;
    throw e;
  }
  window.location.replace(next);
  return null;
}

/** What a failed login or welcome tells the person. */
export function said(err: unknown): string {
  if (err instanceof Refused && err.status === 429 && err.retryAfter !== null) {
    return `Too many attempts for this e-mail. Try again in ${err.retryAfter} ${plural(err.retryAfter, "second")}.`;
  }
  return err instanceof Error ? err.message : String(err);
}

export function Lone({ title, children }: { title: string; children: ReactNode }) {
  useEffect(() => {
    document.title = `${title} - Davy`;
  }, [title]);
  return (
    <main className="lone">
      <div className="lone-brand">
        <Brand />
        <span>Davy</span>
      </div>
      <h1>{title}</h1>
      {children}
    </main>
  );
}

export function Login() {
  const { next } = route.useSearch();
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    const body: LoginBody = { email: String(form.get("email")), password: String(form.get("password")) };
    if (next) body.next = next;
    setBusy(true);
    setProblem(null);
    try {
      const done = await post<LoggedIn>("/login", body);
      setProblem(await enter(done.next));
    } catch (err) {
      setProblem(said(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Lone title="Log in">
      <form className="form" onSubmit={submit}>
        <label htmlFor="email">E-mail</label>
        <input id="email" name="email" type="email" autoComplete="username" required autoFocus />
        <label htmlFor="password">Password</label>
        <input id="password" name="password" type="password" autoComplete="current-password" required />
        {problem ? (
          <p className="problem" role="alert">
            {problem}
          </p>
        ) : null}
        <button type="submit" className="btn primary" disabled={busy}>
          {busy ? "Logging in" : "Log in"}
        </button>
      </form>
      <p className="hint">
        No account yet? Someone with shell access runs <code>davy user add you@example.com</code> and sends you the link it prints.
      </p>
    </Lone>
  );
}
