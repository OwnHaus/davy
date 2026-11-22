import { useState, type FormEvent } from "react";
import { post } from "../api/client";
import type { Me, Welcome as WelcomeBody } from "../api/types";
import { Lone, enter, said } from "./Login";

let secret = "";

/** Called by main.tsx with the part of the link after `#`. */
export function keepSecret(s: string) {
  secret = s;
}

export function Welcome() {
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (!secret) {
    return (
      <Lone title="Welcome">
        <p className="problem" role="alert">
          This page needs the whole link that <code>davy user add</code> printed, including the part after <code>#</code>. Open that link again.
        </p>
      </Lone>
    );
  }

  const submit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    const password = String(form.get("password"));
    if (password !== String(form.get("again"))) {
      setProblem("The two passwords differ.");
      return;
    }
    const body: WelcomeBody = { secret, password };
    setBusy(true);
    setProblem(null);
    try {
      // The answer is the person, and the cookie it sets logs them in.
      await post<Me>("/welcome", body);
      secret = "";
      setProblem(await enter("/ui/"));
    } catch (err) {
      setProblem(said(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Lone title="Welcome to Davy">
      <p>Pick a password for your account. The link works once.</p>
      <form className="form" onSubmit={submit}>
        <label htmlFor="password">New password</label>
        <input id="password" name="password" type="password" autoComplete="new-password" required autoFocus />
        <label htmlFor="again">The same password again</label>
        <input id="again" name="again" type="password" autoComplete="new-password" required />
        {problem ? (
          <p className="problem" role="alert">
            {problem}
          </p>
        ) : null}
        <button type="submit" className="btn primary" disabled={busy}>
          {busy ? "Saving" : "Save and log in"}
        </button>
      </form>
    </Lone>
  );
}
