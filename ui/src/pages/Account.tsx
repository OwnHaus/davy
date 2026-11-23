import { useQuery } from "@tanstack/react-query";
import { useEffect, useState, type FormEvent } from "react";
import { Refused, get, post } from "../api/client";
import type { Me, NewPassword } from "../api/types";
import { plural } from "../lib/text";
import { Shell, type KeyHint } from "./Shell";

const KEYS: KeyHint[] = [];

/** The server checks the same length, this only says it sooner. */
const MIN = 12;

function said(err: unknown): string {
  if (err instanceof Refused && err.status === 429 && err.retryAfter !== null) {
    return `Too many attempts. Try again in ${err.retryAfter} ${plural(err.retryAfter, "second")}.`;
  }
  return err instanceof Error ? err.message : String(err);
}

export function AccountPage() {
  const me = useQuery({ queryKey: ["me"], queryFn: () => get<Me>("/me"), staleTime: Infinity });

  useEffect(() => {
    document.title = "Account - Davy";
  }, []);

  return (
    <Shell keys={KEYS} inspector={false}>
      <main className="page" aria-label="Account">
        <div className="col narrow">
          <h1 className="ph">Account</h1>
          <dl className="kv tonal">
            <dt>E-mail</dt>
            <dd>{me.data?.email ?? ""}</dd>
          </dl>
          <PasswordForm email={me.data?.email ?? ""} />
          <OtherSessions />
        </div>
      </main>
    </Shell>
  );
}

function PasswordForm({ email }: { email: string }) {
  const [problem, setProblem] = useState<string | null>(null);
  const [changed, setChanged] = useState(false);
  const [busy, setBusy] = useState(false);

  const submit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = e.currentTarget;
    const data = new FormData(form);
    const body: NewPassword = { current: String(data.get("current")), new: String(data.get("new")) };
    setChanged(false);
    if (body.new.length < MIN) return setProblem(`The new password needs at least ${MIN} characters.`);
    if (body.new !== String(data.get("again"))) return setProblem("The new password and its repeat differ.");
    setBusy(true);
    setProblem(null);
    try {
      await post("/account/password", body);
      form.reset();
      setChanged(true);
    } catch (err) {
      setProblem(said(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="sec gap" aria-labelledby="pw">
      <h3 id="pw">Change the password</h3>
      <form className="form" onSubmit={submit} noValidate>
        <input type="email" name="username" autoComplete="username" value={email} hidden readOnly />
        <label htmlFor="current">Current password</label>
        <input id="current" name="current" type="password" autoComplete="current-password" required />
        <label htmlFor="new">New password</label>
        <input id="new" name="new" type="password" autoComplete="new-password" minLength={MIN} required aria-describedby="new-hint" />
        <span id="new-hint" className="hint">
          At least {MIN} characters.
        </span>
        <label htmlFor="again">New password again</label>
        <input id="again" name="again" type="password" autoComplete="new-password" required />
        {problem ? (
          <p className="problem" role="alert">
            {problem}
          </p>
        ) : null}
        <p className="said" role="status">
          {changed ? "Password changed. Every other session is logged out." : ""}
        </p>
        <button type="submit" className="btn primary" disabled={busy}>
          {busy ? "Changing" : "Change password"}
        </button>
      </form>
    </section>
  );
}

function OtherSessions() {
  const [state, setState] = useState<"idle" | "busy" | "done">("idle");
  const [problem, setProblem] = useState<string | null>(null);
  const out = async () => {
    setState("busy");
    setProblem(null);
    try {
      await post("/account/logout-others", {});
      setState("done");
    } catch (err) {
      setProblem(said(err));
      setState("idle");
    }
  };
  return (
    <section className="sec gap" aria-labelledby="others">
      <h3 id="others">Other sessions</h3>
      <p className="none">Logs out every browser where this account is logged in, except this one.</p>
      <div className="acts">
        <button type="button" className="btn" disabled={state === "busy"} onClick={() => void out()}>
          Log out every other session
        </button>
      </div>
      {problem ? (
        <p className="problem" role="alert">
          {problem}
        </p>
      ) : null}
      <p className="said" role="status">
        {state === "done" ? "Every other session is logged out." : ""}
      </p>
    </section>
  );
}
