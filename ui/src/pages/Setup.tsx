import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useRef, type ReactNode } from "react";
import type { First, Step } from "../api/types";
import { announce } from "../lib/live";
import { stamp } from "../lib/time";
import { Icon } from "../parts/Icon";
import { LitWindow } from "../parts/Window";
import { connectQuery } from "./Projects";
import { Shell, setupQuery, type KeyHint } from "./Shell";

const KEYS: KeyHint[] = [];

const TITLES: Record<First, string> = {
  agent: "Connect an agent",
  project: "Create a project",
  error: "Send the first error",
  symbolicated: "Read a stack with your source",
  log: "Send a log",
  monitor: "Add a monitor",
  delivered: "Deliver an alert",
};

const toProjects = <Link to="/projects">Projects</Link>;

function todo(what: First, url: string): ReactNode {
  switch (what) {
    case "agent":
      return (
        <>
          <p>Give a coding agent the admin token, then run this in the project it works on. {toProjects} shows each step under Agents.</p>
          <pre className="cmd">davy mcp-config --client claude-code --url {url}</pre>
        </>
      );
    case "project":
      return (
        <>
          <p>A project holds the errors and logs of one app. Create one on the server.</p>
          <pre className="cmd">davy project create my-app --data /var/lib/davy</pre>
        </>
      );
    case "error":
      return <p>Give the project&apos;s DSN to a Sentry SDK and throw an error. {toProjects} has the code for Node, the browser, Python and Go.</p>;
    case "symbolicated":
      return <p>Upload the source maps of a release, then send an error from it. {toProjects} shows the upload under Browser.</p>;
    case "log":
      return <p>Send a log with the project key, from a Logtail shipper, OpenTelemetry or curl. {toProjects} has each one.</p>;
    case "monitor":
      return (
        <>
          <p>Davy checks a URL every minute, or expects a cron job to ping it.</p>
          <pre className="cmd">davy monitor add my-app home --url https://example.com/health --data /var/lib/davy</pre>
        </>
      );
    case "delivered":
      return (
        <>
          <p>Add a channel, then send it a test notice.</p>
          <pre className="cmd">{"davy channel add slack <webhook-url> --all --data /var/lib/davy\ndavy channel test 1 --data /var/lib/davy"}</pre>
        </>
      );
  }
}

export function SetupPage() {
  const setup = useQuery({ ...setupQuery, refetchInterval: 2000, refetchIntervalInBackground: false, staleTime: 0 });
  const connect = useQuery(connectQuery);
  const seen = useRef<Set<First> | null>(null);
  const steps = setup.data?.steps ?? [];
  const required = steps.filter((s) => s.required);
  const done = required.filter((s) => s.at !== null).length;

  useEffect(() => {
    document.title = "Setup - Davy";
  }, []);

  useEffect(() => {
    if (!setup.data) return;
    const now = new Set(setup.data.steps.filter((s) => s.at !== null).map((s) => s.what));
    const before = seen.current;
    seen.current = now;
    if (!before) return;
    const fresh = [...now].filter((w) => !before.has(w));
    if (fresh.length) announce(`Done: ${fresh.map((w) => TITLES[w]).join(", ")}`);
  }, [setup.data]);

  return (
    <Shell keys={KEYS} inspector={false}>
      <main className="page" aria-label="Setup">
        <div className="col">
          <div className="setup-head">
            <LitWindow height={56} />
            <h1 className="ph">Set up Davy</h1>
          </div>
          {setup.error ? (
            <p className="none">{setup.error.message}</p>
          ) : !setup.data ? (
            <p className="none">Loading setup</p>
          ) : (
            <>
              <p className="lede">
                {done} of {required.length} required steps done.
                {done === required.length ? (
                  <>
                    {" "}
                    Davy is ready. <Link to="/issues">Go to issues</Link>
                  </>
                ) : (
                  " This page checks again every two seconds."
                )}
              </p>
              <ol className="steps">
                {steps.map((s, i) => (
                  <StepItem key={s.what} step={s} n={i + 1} url={connect.data?.public_url ?? "<public-url>"} />
                ))}
              </ol>
            </>
          )}
        </div>
      </main>
    </Shell>
  );
}

function StepItem({ step, n, url }: { step: Step; n: number; url: string }) {
  const done = step.at !== null;
  return (
    <li className={done ? "done" : undefined}>
      <span className="num" aria-hidden="true">
        {done ? <Icon name="check" /> : n}
      </span>
      <div>
        <h2>
          {TITLES[step.what]}
          {step.required ? null : <span>optional</span>}
        </h2>
        {step.at !== null ? (
          <p className="none">
            Done <span className="mono">{stamp(step.at)}</span>
          </p>
        ) : (
          <>
            <p className="none">{step.required ? "Not done yet." : "Not set up."}</p>
            {todo(step.what, url)}
          </>
        )}
      </div>
    </li>
  );
}
