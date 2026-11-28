import { useMutation, useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { get, post, queries } from "../api/client";
import type { AgentToken, Level, TokenChange } from "../api/types";
import { useNow } from "../lib/live";
import { plural } from "../lib/text";
import { agoLong, stamp } from "../lib/time";
import { Shell, type KeyHint } from "./Shell";

const KEYS: KeyHint[] = [];

const LEVELS: [Level, string][] = [
  ["read", "Read"],
  ["triage", "Triage"],
  ["full", "Full"],
];

/** What each level lets a token do, as "Levels" in docs/design.md says. */
const REACH: Record<Level, string> = {
  read: "Every tool that only reads",
  triage: "Also resolves issues, sends feedback and stages shadow drop rules",
  full: "Also keeps data longer",
};

const tokensQuery = { queryKey: ["tokens"], queryFn: () => get<AgentToken[]>("/tokens"), refetchInterval: 30_000 };

/** A row's open question: unlock or revoke, before it is sent. */
type Asking = { name: string; what: "unlock" | "revoke" } | null;

export function Tokens() {
  const now = useNow();
  const tokens = useQuery(tokensQuery);
  const [asking, setAsking] = useState<Asking>(null);
  const change = useMutation({
    mutationFn: (body: TokenChange) => post<AgentToken[]>("/tokens", body),
    onSuccess: (all) => {
      setAsking(null);
      queries.setQueryData(tokensQuery.queryKey, all);
      void queries.invalidateQueries({ queryKey: ["ledger"] });
    },
  });
  const all = tokens.data ?? [];
  const live = all.filter((t) => t.revoked_at === null);
  const revoked = all.filter((t) => t.revoked_at !== null);

  useEffect(() => {
    document.title = "Tokens - Davy";
  }, []);

  return (
    <Shell keys={KEYS} inspector={false}>
      <main className="page" aria-label="Agent tokens" aria-busy={tokens.isFetching || change.isPending}>
        <h1 className="ph">Agent tokens</h1>
        <p className="lede">
          Each agent connects to MCP with a token of its own. Its level says what it may change. Destructive changes stay locked until a person unlocks them for one token of level full.{" "}
          <Link to="/ledger">The ledger</Link> shows what each token did.
        </p>
        <section className="sec" aria-labelledby="live">
          <h3 id="live">
            Live<span>{live.length ? `${live.length} ${plural(live.length, "token")}` : ""}</span>
          </h3>
          {tokens.error ? (
            <p className="none">{tokens.error.message}</p>
          ) : tokens.isPending ? (
            <p className="none">Loading tokens</p>
          ) : live.length === 0 ? (
            <NoTokens />
          ) : (
            <LiveTokens tokens={live} now={now} asking={asking} busy={change.isPending} onAsk={setAsking} onChange={(body) => change.mutate(body)} />
          )}
          {change.error ? (
            <p className="problem" role="alert">
              {change.error.message}
            </p>
          ) : null}
        </section>
        {revoked.length ? (
          <section className="sec" aria-labelledby="revoked">
            <h3 id="revoked">
              Revoked<span>kept 180 days, so the ledger still names them</span>
            </h3>
            <RevokedTokens tokens={revoked} />
          </section>
        ) : null}
      </main>
    </Shell>
  );
}

function NoTokens() {
  return (
    <div className="none">
      <p>No agent tokens yet. Add one on the server for each agent. It prints the secret once.</p>
      <pre className="cmd">davy token add ci-agent --level triage --data /var/lib/davy</pre>
      <p>Until then agents connect with the admin token, which reaches every tool but the destructive ones.</p>
    </div>
  );
}

type RowProps = {
  tokens: AgentToken[];
  now: number;
  asking: Asking;
  busy: boolean;
  onAsk: (a: Asking) => void;
  onChange: (body: TokenChange) => void;
};

function LiveTokens({ tokens, now, asking, busy, onAsk, onChange }: RowProps) {
  return (
    <div className="tscroll">
      <table className="iss plain">
        <colgroup>
          <col className="w-token" />
          <col className="w-levels" />
          <col />
          <col className="w-used" />
          <col className="w-acts" />
        </colgroup>
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Level</th>
            <th scope="col">Destructive changes</th>
            <th scope="col">Last used</th>
            <th scope="col">
              <span className="sr-only">Revoke</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {tokens.map((t) => (
            <TokenRow key={t.name} token={t} now={now} asking={asking?.name === t.name ? asking.what : null} busy={busy} onAsk={onAsk} onChange={onChange} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

type TokenRowProps = Omit<RowProps, "tokens" | "asking"> & { token: AgentToken; asking: "unlock" | "revoke" | null };

function TokenRow({ token: t, now, asking, busy, onAsk, onChange }: TokenRowProps) {
  const name = t.name;
  return (
    <tr>
      <th scope="row" className="mono" title={`added ${stamp(t.created_at)}`}>
        {name}
      </th>
      <td>
        <div className="seg" role="group" aria-label={`Level of ${name}`}>
          {LEVELS.map(([level, label]) => (
            <button
              key={level}
              type="button"
              data-text={label}
              aria-pressed={t.level === level}
              title={REACH[level]}
              disabled={busy}
              onClick={() => t.level !== level && onChange({ do: "level", name, level })}
            >
              {label}
            </button>
          ))}
        </div>
      </td>
      <td>
        {asking === "unlock" ? (
          <Confirm
            says={`${name} may then shorten retention and make drop rules live, which deletes data.`}
            yes="Unlock"
            busy={busy}
            onYes={() => onChange({ do: "unlock", name })}
            onNo={() => onAsk(null)}
          />
        ) : (
          <Destructive token={t} now={now} busy={busy} onUnlock={() => onAsk({ name, what: "unlock" })} onLock={() => onChange({ do: "lock", name })} />
        )}
      </td>
      <td className="rel" title={t.used_at ? stamp(t.used_at) : undefined}>
        {t.used_at ? agoLong(t.used_at, now) : <span className="dim">never</span>}
      </td>
      <td className="r">
        {asking === "revoke" ? (
          <Confirm says={`Revoke ${name}?`} yes="Revoke" busy={busy} onYes={() => onChange({ do: "revoke", name })} onNo={() => onAsk(null)} />
        ) : (
          <button type="button" className="btn" disabled={busy} onClick={() => onAsk({ name, what: "revoke" })}>
            Revoke
          </button>
        )}
      </td>
    </tr>
  );
}

function Destructive({ token: t, now, busy, onUnlock, onLock }: { token: AgentToken; now: number; busy: boolean; onUnlock: () => void; onLock: () => void }) {
  if (t.destructive) {
    return (
      <span className="inline">
        <span className="problem">unlocked</span>
        <span className="dim" title={t.unlocked_at ? stamp(t.unlocked_at) : undefined}>
          by {t.unlocked_by ?? "someone"}
          {t.unlocked_at ? `, ${agoLong(t.unlocked_at, now)}` : ""}
        </span>
        <button type="button" className="btn" disabled={busy} onClick={onLock}>
          Lock
        </button>
      </span>
    );
  }
  const full = t.level === "full";
  return (
    <span className="inline">
      <span>locked</span>
      <button type="button" className="btn" disabled={busy || !full} title={full ? undefined : "Only a token of level full can be unlocked"} onClick={onUnlock}>
        Unlock
      </button>
    </span>
  );
}

function Confirm({ says, yes, busy, onYes, onNo }: { says: string; yes: string; busy: boolean; onYes: () => void; onNo: () => void }) {
  return (
    <span className="inline" role="group" aria-label={says}>
      <span>{says}</span>
      <button type="button" className="btn primary" disabled={busy} onClick={onYes}>
        {yes}
      </button>
      <button type="button" className="btn" disabled={busy} autoFocus onClick={onNo}>
        Cancel
      </button>
    </span>
  );
}

function RevokedTokens({ tokens }: { tokens: AgentToken[] }) {
  return (
    <div className="tscroll">
      <table className="iss plain">
        <colgroup>
          <col className="w-token" />
          <col className="w-kind" />
          <col className="w-when" />
          <col />
        </colgroup>
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Level</th>
            <th scope="col">Revoked</th>
            <th scope="col">Last used</th>
          </tr>
        </thead>
        <tbody>
          {tokens.map((t) => (
            <tr key={t.name} className="dim">
              <th scope="row" className="mono">
                {t.name}
              </th>
              <td>{t.level}</td>
              <td className="mono">{t.revoked_at ? stamp(t.revoked_at) : ""}</td>
              <td className="mono">{t.used_at ? stamp(t.used_at) : "never"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
