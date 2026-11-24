(() => {
  'use strict';

  const reduce = matchMedia('(prefers-reduced-motion: reduce)');

  /* ---------- copy buttons ---------- */
  function copyText(text) {
    if (navigator.clipboard && window.isSecureContext) return navigator.clipboard.writeText(text);
    return new Promise((resolve, reject) => {
      const ta = document.createElement('textarea');
      ta.value = text;
      ta.setAttribute('readonly', '');
      ta.style.position = 'fixed';
      ta.style.opacity = '0';
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand('copy');
      ta.remove();
      ok ? resolve() : reject(new Error('copy failed'));
    });
  }
  document.querySelectorAll('[data-copy]').forEach((btn) => {
    const label = btn.querySelector('span');
    let t;
    btn.addEventListener('click', () => {
      const src = document.getElementById(btn.dataset.copy);
      if (!src) return;
      copyText(src.textContent.trim() + '\n').then(() => {
        label.textContent = 'Copied';
        btn.classList.add('done');
      }, () => {
        label.textContent = 'Select and copy';
      });
      clearTimeout(t);
      t = setTimeout(() => { label.textContent = 'Copy'; btn.classList.remove('done'); }, 1800);
    });
  });

  /* ---------- install tabs ---------- */
  const tabs = document.querySelector('.tabs');
  if (tabs) {
    const list = tabs.querySelector('[role="tablist"]');
    const buttons = [...list.querySelectorAll('[role="tab"]')];
    const panels = buttons.map((b) => document.getElementById(b.getAttribute('aria-controls')));
    function select(i, focus) {
      buttons.forEach((b, j) => {
        const on = i === j;
        b.setAttribute('aria-selected', String(on));
        b.tabIndex = on ? 0 : -1;
        panels[j].hidden = !on;
      });
      if (focus) buttons[i].focus();
    }
    list.hidden = false;
    tabs.classList.add('ready');
    panels.forEach((p) => { p.tabIndex = 0; });
    select(0, false);
    buttons.forEach((b, i) => {
      b.addEventListener('click', () => select(i, false));
      b.addEventListener('keydown', (e) => {
        const n = buttons.length;
        let k = null;
        if (e.key === 'ArrowRight') k = (i + 1) % n;
        else if (e.key === 'ArrowLeft') k = (i - 1 + n) % n;
        else if (e.key === 'Home') k = 0;
        else if (e.key === 'End') k = n - 1;
        if (k !== null) { e.preventDefault(); select(k, true); }
      });
    });
  }

  /* ---------- the flame: one damp() for every lamp on the page ---------- */
  const lamps = () => [...document.querySelectorAll('.lamp, .mark, .sim-lamp')];
  let dampTimer;
  function damp() {
    lamps().forEach((x) => x.classList.add('damp'));
    clearTimeout(dampTimer);
    dampTimer = setTimeout(() => lamps().forEach((x) => x.classList.remove('damp')), 4500);
  }

  const two = (x) => String(x).padStart(2, '0');
  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function row(list, max, { title, meta, time, result, cls }) {
    const li = el('li', cls || '');
    li.appendChild(el('span', 't', title));
    const m = el('span', 'm');
    m.appendChild(el('time', null, time));
    m.appendChild(document.createTextNode(meta));
    li.appendChild(m);
    li.appendChild(el('span', 'r', result));
    list.prepend(li);
    if (!reduce.matches) {
      [...list.children].forEach((c) => {
        c.classList.remove('enter');
        void c.offsetWidth;
        c.classList.add('enter');
      });
    }
    setTimeout(() => {
      while (list.children.length > max) list.lastElementChild.remove();
    }, reduce.matches ? 0 : 460);
  }

  /* ---------- section nav: the current section, the contents panel ---------- */
  const navLinks = [...document.querySelectorAll('.nav a, .toc-list a')];
  const sections = [...new Set(navLinks.map((a) => a.getAttribute('href')))]
    .map((h) => document.querySelector(h)).filter(Boolean);
  if ('IntersectionObserver' in window && sections.length) {
    const seen = new Map();
    const io = new IntersectionObserver((entries) => {
      entries.forEach((e) => seen.set(e.target.id, e.isIntersecting));
      const current = sections.find((x) => seen.get(x.id));
      navLinks.forEach((a) => {
        if (current && a.getAttribute('href') === '#' + current.id) a.setAttribute('aria-current', 'true');
        else a.removeAttribute('aria-current');
      });
    }, { rootMargin: '-35% 0px -60% 0px' });
    sections.forEach((x) => io.observe(x));
  }
  const toc = document.querySelector('.toc');
  if (toc) {
    const close = () => { if (toc.open) toc.open = false; };
    toc.querySelectorAll('.toc-list a').forEach((a) => a.addEventListener('click', close));
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape' && toc.open) { close(); toc.querySelector('summary').focus(); }
    });
    document.addEventListener('click', (e) => { if (!toc.contains(e.target)) close(); });
  }

  /* ---------- screenshots open the full size in the current scheme ---------- */
  const darkMq = matchMedia('(prefers-color-scheme: dark)');
  const shotLinks = [...document.querySelectorAll('.shot-link')];
  function shotHrefs() {
    shotLinks.forEach((a) => {
      if (!a.dataset.light) a.dataset.light = a.getAttribute('href');
      a.setAttribute('href', darkMq.matches ? a.dataset.dark : a.dataset.light);
    });
  }
  shotHrefs();
  darkMq.addEventListener?.('change', shotHrefs);

  /* ---------- the simulator: IssueStatus::on_event from src/store.rs ---------- */
  const sim = document.querySelector('.sim');
  if (sim) {
    const MIN = 60000;
    const at = (d, h, m, s) => Date.UTC(2026, 10, d, h, m, s || 0);
    const START = () => ({
      clock: at(24, 10, 5),
      seen: { '2.4.1': at(22, 14, 9, 51), '2.4.2': at(23, 13, 58, 30), '2.5.0': at(24, 9, 12, 44), '2.5.1': at(24, 9, 58, 10), '2.6.0': null },
      status: 'resolved', inRelease: 'pantry-api@2.5.1', resolvedAt: at(24, 10, 2), regressedAt: at(24, 9, 12, 44), count: 86,
    });
    let st = START();
    const TITLE = "TypeError: Cannot read properties of undefined (reading 'servings')";
    const q = (s) => sim.querySelector(s);
    const f = (k) => sim.querySelector(`[data-f="${k}"]`);
    const log = q('[data-sim="log"]');
    const clockEl = q('[data-sim="clock"]');
    const verdict = q('[data-sim="verdict"]');
    const relBtns = [...sim.querySelectorAll('.rel')];
    const fixBtns = [...sim.querySelectorAll('.fix')];
    const mon = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
    const d = (t) => new Date(t);
    const hm = (t) => `${two(d(t).getUTCHours())}:${two(d(t).getUTCMinutes())}`;
    const hms = (t) => `${hm(t)}:${two(d(t).getUTCSeconds())}`;
    const day = (t) => `${mon[d(t).getUTCMonth()]} ${d(t).getUTCDate()}`;
    const when = (t, ref) => (day(t) === day(ref) ? hm(t) : `${day(t)}, ${hm(t)}`);
    const iso = (t) => (t == null ? 'null' : d(t).toISOString());

    function setField(k, v) {
      const n = f(k);
      const text = String(v);
      n.classList.toggle('null', text === 'null');
      if (n.textContent === text) return;
      n.textContent = text;
      n.classList.add('changed');
      setTimeout(() => n.classList.remove('changed'), 1600);
    }
    function render() {
      clockEl.textContent = `${day(st.clock)}, ${hms(st.clock)} UTC`;
      clockEl.setAttribute('datetime', iso(st.clock));
      setField('status', st.status);
      setField('in', st.inRelease ?? 'null');
      setField('at', iso(st.resolvedAt));
      setField('reg', iso(st.regressedAt));
      setField('n', st.count);
      relBtns.forEach((b) => {
        const v = b.dataset.rel;
        if (!v) return;
        const seen = st.seen[v];
        b.querySelector('span').textContent = seen == null ? 'not seen yet' : `first seen ${day(seen)}, ${hm(seen)}`;
      });
      fixBtns.forEach((b) => { b.disabled = st.status === 'resolved'; });
    }
    function say(html, back) {
      verdict.innerHTML = html;
      verdict.classList.toggle('is-back', !!back);
    }

    function send(v) {
      st.clock += 3 * MIN + 17000;
      const now = st.clock;
      const name = v ? `pantry-api@${v}` : null;
      let isNew = false;
      if (v && st.seen[v] == null) { st.seen[v] = now; isNew = true; }   // the release row is written before on_event
      st.count += 1;
      let reopens = false, why = '';
      if (st.status === 'resolved') {
        if (!v) { reopens = true; why = 'The event has no release, so Davy cannot tell it from the fixed code.'; }
        else if (st.inRelease === name) { reopens = true; why = `${v} is the release the fix shipped in, so the fix did not work.`; }
        else if (st.seen[v] > st.resolvedAt) {
          reopens = true;
          why = isNew ? `Davy sees ${v} for the first time now, after the fix at ${when(st.resolvedAt, now)}.`
                      : `Davy first saw ${v} at ${when(st.seen[v], now)}, after the fix at ${when(st.resolvedAt, now)}.`;
        } else {
          why = `Davy first saw ${v} at ${when(st.seen[v], now)}, before the fix at ${when(st.resolvedAt, now)}`
              + (st.inRelease ? `, and the fix shipped in ${st.inRelease.slice(11)}.` : '.');
        }
      }
      relBtns.forEach((b) => b.classList.toggle('sent', b.dataset.rel === v));
      relBtns.forEach((b) => b.classList.toggle('new-seen', isNew && b.dataset.rel === v));
      let result, cls;
      if (reopens) {
        st.status = 'unresolved'; st.regressedAt = now; st.resolvedAt = null; st.inRelease = null;
        result = '#1 regressed · notice sent'; cls = 'is-back';
        say(`${why} <b>Issue 1 reopens</b> and sends a notice.`, true);
        damp();
      } else if (st.status === 'resolved') {
        result = `#1 stays fixed · ${st.count}`; cls = 'is-fixed';
        say(`${why} <b>Stays resolved</b>, counted, no notice.`);
      } else {
        result = `#1 · ${st.count}`;
        say('The issue is already open. The event is counted and sends no new notice. Mark it fixed to arm the rule again.');
      }
      row(log, 4, { title: TITLE, time: hms(now), meta: name ? `pantry-api · ${v}` : 'pantry-api · no release', result, cls });
      render();
    }

    function fix(v) {
      st.clock += 2 * MIN + 41000;
      const now = st.clock;
      st.status = 'resolved'; st.resolvedAt = now; st.inRelease = v ? `pantry-api@${v}` : null;
      const call = v ? `mark_fixed {"issue_id": 1, "release": "pantry-api@${v}"}` : 'mark_fixed {"issue_id": 1}';
      const reply = v ? `issue 1 resolved in release pantry-api@${v}` : 'issue 1 resolved until a new release reports it';
      row(log, 4, { title: call, time: hms(now), meta: 'agent over MCP', result: reply.replace('issue 1 ', ''), cls: 'is-fixed' });
      say(v ? `Resolved at ${hm(now)} in ${v}. Now an event from ${v}, from a release first seen after ${hm(now)}, or without a release reopens it.`
            : `Resolved at ${hm(now)} with no release. Only a release Davy first sees after ${hm(now)}, or an event without a release, reopens it.`);
      relBtns.forEach((b) => b.classList.remove('sent', 'new-seen'));
      render();
    }

    relBtns.forEach((b) => b.addEventListener('click', () => send(b.dataset.rel)));
    fixBtns.forEach((b) => b.addEventListener('click', () => {
      fix(b.dataset.fix);
      relBtns[0].focus();
    }));
    q('[data-sim="reset"]').addEventListener('click', () => {
      st = START();
      log.replaceChildren();
      relBtns.forEach((b) => b.classList.remove('sent', 'new-seen'));
      say('Issue 1 is resolved in pantry-api@2.5.1. Pick a release.');
      render();
    });
    render();
  }

  /* ---------- the ledger: a demo stream of invented events ---------- */
  const ledger = document.querySelector('.ledger');
  if (!ledger) return;
  const rows = ledger.querySelector('.rows');
  const num = {
    events: ledger.querySelector('[data-n="events"]'),
    issues: ledger.querySelector('[data-n="issues"]'),
    notices: ledger.querySelector('[data-n="notices"]'),
  };
  const pauseBtn = ledger.querySelector('[data-act="pause"]');
  const throwBtn = ledger.querySelector('[data-act="throw"]');
  const live = el('p', 'sr');
  live.setAttribute('aria-live', 'polite');
  ledger.appendChild(live);

  // the seven issues of the demo data, with their event counts; weight is how often they recur
  const issues = new Map([
    [1, { title: "TypeError: Cannot read properties of undefined (reading 'servings')", proj: 'pantry-api', n: 84, w: 5 }],
    [2, { title: 'Error: connect ETIMEDOUT 10.0.3.12:5432', proj: 'pantry-api', n: 67, w: 3 }],
    [3, { title: 'RangeError: Invalid time value', proj: 'pantry-web', n: 60, w: 3 }],
    [4, { title: `SyntaxError: Unexpected token '<', "<!DOCTYPE "... is not valid JSON`, proj: 'pantry-web', n: 18, w: 1 }],
    [5, { title: "KeyError: 'unit'", proj: 'pantry-worker', n: 24, w: 1 }],
    [6, { title: 'ValidationError: quantity must be a positive number', proj: 'pantry-api', n: 22, w: 1 }],
    [7, { title: "QuotaExceededError: Failed to execute 'setItem' on 'Storage': Setting the value of 'pantry:list' exceeded the quota.", proj: 'pantry-web', n: 6, w: 1 }],
  ]);
  const pool = [
    { title: 'TypeError: list.items is not iterable', proj: 'pantry-web' },
    { title: 'TimeoutError: Navigation timeout of 30000 ms exceeded', proj: 'pantry-worker' },
    { title: 'ZeroDivisionError: division by zero', proj: 'pantry-worker' },
    { title: "PermissionError: [Errno 13] Permission denied: '/srv/exports'", proj: 'pantry-worker' },
    { title: 'Error: getaddrinfo ENOTFOUND smtp.pantry.example', proj: 'pantry-api' },
  ];
  let poolAt = 0;
  let nextId = 8;
  let untilNew = 6;
  let events = 281, issueCount = 7, notices = 0;
  let clock = 13 * 3600 + 32 * 60 + 57;
  function tick() {
    clock += 2 + Math.floor(Math.random() * 8);
    return `${two(Math.floor(clock / 3600) % 24)}:${two(Math.floor(clock / 60) % 60)}:${two(clock % 60)}`;
  }

  const bumpTimers = new WeakMap();
  function set(key, value) {
    const n = num[key];
    if (n.textContent === String(value)) return;
    n.textContent = value;
    if (key === 'events') return;
    n.classList.add('bump');
    clearTimeout(bumpTimers.get(n));
    bumpTimers.set(n, setTimeout(() => n.classList.remove('bump'), 1400));
  }

  function pick() {
    const all = [...issues.entries()];
    let x = Math.random() * all.reduce((s, [, v]) => s + v.w, 0);
    for (const [id, v] of all) { x -= v.w; if (x <= 0) return id; }
    return all[0][0];
  }

  function normal() {
    const id = pick();
    const v = issues.get(id);
    v.n += 1;
    events += 1;
    row(rows, 6, { title: v.title, meta: v.proj, time: tick(), result: `#${id} · ${v.n}` });
    set('events', events);
  }

  function newIssue() {
    const p = pool[poolAt++ % pool.length];
    const id = nextId++;
    issues.set(id, { title: p.title, proj: p.proj, n: 1, w: 1 });
    events += 1; issueCount += 1; notices += 1;
    row(rows, 6, { title: p.title, meta: p.proj, time: tick(), result: `#${id} new · notice sent`, cls: 'is-new' });
    set('events', events); set('issues', issueCount); set('notices', notices);
    live.textContent = `Demo: new issue #${id}. A notice went out.`;
    damp();
  }

  function step() {
    if (untilNew > 0) { untilNew -= 1; normal(); return; }
    newIssue();
    untilNew = 6 + Math.floor(Math.random() * 4);
  }

  let timer = null;
  let userPaused = reduce.matches;
  function schedule() {
    clearTimeout(timer);
    timer = null;
    if (userPaused || document.hidden) return;
    timer = setTimeout(() => { step(); schedule(); }, 1300 + Math.random() * 700);
  }
  function setPaused(p) {
    userPaused = p;
    pauseBtn.setAttribute('aria-pressed', String(p));
    pauseBtn.querySelector('span').textContent = p ? 'Play' : 'Pause';
    schedule();
  }
  pauseBtn.addEventListener('click', () => setPaused(!userPaused));
  throwBtn.addEventListener('click', () => {
    newIssue();
    untilNew = 6;
  });
  document.addEventListener('visibilitychange', schedule);
  reduce.addEventListener?.('change', (e) => { if (e.matches) setPaused(true); });
  setPaused(userPaused);
})();
