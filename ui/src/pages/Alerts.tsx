import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { useEffect } from "react";
import { get, query } from "../api/client";
import type { Channel, Kind, Notification, NotificationPage } from "../api/types";
import { useNow } from "../lib/live";
import { plural, shown } from "../lib/text";
import { ago, stamp } from "../lib/time";
import { DeliveryMark } from "../parts/Marks";
import { RailButton, Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/alerts");

const KEYS: KeyHint[] = [];

const KINDS: Record<Kind, string> = { webhook: "Webhook", slack: "Slack", email: "E-mail" };

export function Alerts() {
  const { channel } = route.useSearch();
  const navigate = useNavigate();
  const now = useNow();
  const channels = useQuery({ queryKey: ["channels"], queryFn: () => get<Channel[]>("/channels"), refetchInterval: 30_000 });
  const sent = useInfiniteQuery({
    queryKey: ["notifications", channel ?? null],
    queryFn: ({ pageParam }) => get<NotificationPage>(`/notifications${query({ channel, cursor: pageParam })}`),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => page.next ?? undefined,
    refetchInterval: 30_000,
  });
  const all = channels.data ?? [];
  const named = new Map(all.map((c) => [c.id, c]));
  const picked = channel === undefined ? undefined : named.get(Number(channel));
  const rows = sent.data?.pages.flatMap((p) => p.notifications) ?? [];

  useEffect(() => {
    document.title = "Alerts - Davy";
  }, []);

  const pick = (id: number | undefined, close: () => void) => {
    close();
    void navigate({ to: "/alerts", search: id === undefined ? {} : { channel: String(id) } });
  };
  const rail = (close: () => void) => (
    <div className="rgroup">
      <h2>Channels</h2>
      <RailButton label="All channels" on={channel === undefined} onClick={() => pick(undefined, close)} />
      {all.map((c) => (
        <RailButton
          key={c.id}
          label={c.to}
          on={picked?.id === c.id}
          onClick={() => pick(c.id, close)}
          extra={<span className="n">{KINDS[c.kind]}</span>}
        />
      ))}
    </div>
  );

  return (
    <Shell keys={KEYS} rail={rail} inspector={false}>
      <main className="page" aria-label="Alerts" aria-busy={channels.isFetching || sent.isFetching}>
        <h1 className="ph">Alerts</h1>
        <section className="sec" aria-labelledby="channels">
          <h3 id="channels">
            Channels<span>{all.length ? `${all.length} ${plural(all.length, "channel")}` : ""}</span>
          </h3>
          {channels.error ? (
            <p className="none">{channels.error.message}</p>
          ) : channels.isPending ? (
            <p className="none">Loading channels</p>
          ) : all.length === 0 ? (
            <NoChannels />
          ) : (
            <Channels channels={all} />
          )}
        </section>
        <section className="sec" aria-labelledby="sent">
          <h3 id="sent">
            Notifications<span>{picked ? `to ${shown(picked.to)}` : "newest first"}</span>
          </h3>
          {sent.error ? (
            <p className="none">{sent.error.message}</p>
          ) : sent.isPending ? (
            <p className="none">Loading notifications</p>
          ) : rows.length === 0 ? (
            <p className="none">
              {channel !== undefined && !picked ? `There is no channel ${channel}.` : "Nothing was sent yet. A new issue, a regression or a monitor going down sends one."}
            </p>
          ) : (
            <>
              <Notifications rows={rows} named={named} now={now} />
              {sent.hasNextPage ? (
                <div className="acts">
                  <button type="button" className="btn" disabled={sent.isFetchingNextPage} onClick={() => void sent.fetchNextPage()}>
                    {sent.isFetchingNextPage ? "Loading older notifications" : "Load older notifications"}
                  </button>
                </div>
              ) : null}
            </>
          )}
        </section>
      </main>
    </Shell>
  );
}

function NoChannels() {
  return (
    <div className="none">
      <p>No channels yet. Add one on the server, then send a test notice to it.</p>
      <pre className="cmd">
        {"davy channel add slack <webhook-url> --all --data /var/lib/davy\ndavy channel test 1 --data /var/lib/davy"}
      </pre>
      <p>A webhook channel prints its signing secret once. Add --feedback to also get the reports agents send.</p>
    </div>
  );
}

function Channels({ channels }: { channels: Channel[] }) {
  return (
    <div className="tscroll">
      <table className="iss plain">
        <colgroup>
          <col className="w-id" />
          <col className="w-kind" />
          <col />
          <col className="w-project" />
          <col className="w-yes" />
          <col className="w-num" />
          <col className="w-num" />
          <col className="w-num" />
          <col className="w-num-wide" />
        </colgroup>
        <thead>
          <tr>
            <th scope="col" className="r">
              #
            </th>
            <th scope="col">Kind</th>
            <th scope="col">Sends to</th>
            <th scope="col">Project</th>
            <th scope="col">Agent reports</th>
            <th scope="col" className="r">
              Unsent
            </th>
            <th scope="col" className="r">
              Failed
            </th>
            <th scope="col" className="r">
              Dropped
            </th>
            <th scope="col" className="r" title="Notices of monitors made by check-ins. Anyone with the DSN can raise this count.">
              SDK dropped
            </th>
          </tr>
        </thead>
        <tbody>
          {channels.map((c) => (
            <tr key={c.id}>
              <td className="r mono">{c.id}</td>
              <td>{KINDS[c.kind]}</td>
              <td className="fn" title={shown(c.to)}>
                {shown(c.to)}
              </td>
              <td>{c.project === null ? <span className="dim">all projects</span> : shown(c.project)}</td>
              <td>{c.feedback ? "yes" : <span className="dim">no</span>}</td>
              <td className="r">{c.unsent}</td>
              <td className="r">{c.failed ? <span className="problem">{c.failed}</span> : 0}</td>
              <td className="r">{c.dropped ? <span className="problem">{c.dropped}</span> : 0}</td>
              <td className="r">{c.sdk_dropped}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** When it was sent or given up, or when the next try is due. */
function outcome(n: Notification, now: number): string {
  if (n.done) return `${n.delivery === "failed" ? "gave up" : "sent"} ${ago(n.done, now)}`;
  if (!n.next_try) return "";
  const wait = Date.parse(n.next_try) - now;
  return wait <= 0 ? "due now" : `next try in ${Math.ceil(wait / 60_000)}m`;
}

function Notifications({ rows, named, now }: { rows: Notification[]; named: Map<number, Channel>; now: number }) {
  return (
    <div className="tscroll">
      <table className="iss plain">
        <colgroup>
          <col className="w-when" />
          <col className="w-chan" />
          <col className="w-event" />
          <col className="w-status" />
          <col className="w-num" />
          <col className="w-next" />
          <col />
        </colgroup>
        <thead>
          <tr>
            <th scope="col">Created</th>
            <th scope="col">Channel</th>
            <th scope="col">Event</th>
            <th scope="col">Delivery</th>
            <th scope="col" className="r">
              Tries
            </th>
            <th scope="col">Outcome</th>
            <th scope="col">Last error</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((n) => {
            const c = named.get(n.channel);
            return (
              <tr key={n.id}>
                <td className="mono">{stamp(n.created)}</td>
                <td title={c ? shown(c.to) : undefined}>{c ? shown(c.to) : `channel ${n.channel}`}</td>
                <td className="fn">{shown(n.kind)}</td>
                <td>
                  <DeliveryMark delivery={n.delivery} />
                </td>
                <td className="r">{n.attempts}</td>
                <td className="rel" title={n.done ? stamp(n.done) : n.next_try ? stamp(n.next_try) : undefined}>
                  {outcome(n, now)}
                </td>
                <td title={n.last_error ? shown(n.last_error) : undefined}>
                  {n.last_error ? <span className="problem">{shown(n.last_error)}</span> : <span className="dim">none</span>}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
