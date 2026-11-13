/* Overlay client.
 *
 * Connects to the daemon's WebSocket and renders the live snapshot. Every
 * behaviour is configurable by query string so the same page can serve an OBS
 * Browser Source, a small always-on-top desktop window, and a debugging view:
 *
 *   ?panels=meter,buffs   panels: meter, dps, outgoing, xp, heals, taken,
 *                                 incoming, auras, buffs, debuffs, motes,
 *                                 charm, sky, apps (wanted is an alias)
 *   ?rows=6               max meter rows (applies to each meter panel)
 *   ?buffs=8              max buff timers
 *   ?scale=1.25           zoom, for high-resolution captures
 *   ?width=340            no longer used; the window or capture source sets
 *                         the width and the content fills it
 *   ?self=1               show only your own row in the meter
 *   ?enemies=0            hide enemy rows
 *   ?crit=0               hide the crit % column in the meter
 *   ?scope=session        meter the whole session, or ?scope=zone for the zone
 *   ?idle=1               keep the last fight on screen when combat ends
 */

(() => {
  "use strict";

  const params = new URLSearchParams(location.search);
  const opt = {
    // `?? ` not `||`: an absent parameter means "use the defaults", but an
    // empty one means the user turned everything off, and falling back would
    // put panels back on screen that they had just unticked.
    panels: (params.get("panels") ?? "meter,buffs")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean),
    rows: int(params.get("rows"), 6),
    buffs: int(params.get("buffs"), 8),
    scale: float(params.get("scale"), 1),
    width: int(params.get("width"), 340),
    selfOnly: params.get("self") === "1",
    showEnemies: params.get("enemies") !== "0",
    showCrit: params.get("crit") !== "0",
    // "session" or "zone" replaces the per-fight meter with a running total.
    // Anything else, including absent, means the fight in progress.
    scope: ["session", "zone"].includes(params.get("scope")) ? params.get("scope") : null,
    keepIdle: params.get("idle") === "1",
  };

  function int(v, d) {
    const n = parseInt(v, 10);
    return Number.isFinite(n) ? n : d;
  }
  function float(v, d) {
    const n = parseFloat(v);
    return Number.isFinite(n) ? n : d;
  }

  const app = document.getElementById("app");
  /* Fill whatever the page has been given rather than a width of our own.
   *
   * Both consumers already decide the size: an OBS Browser Source is created
   * at a width you choose, and the desktop overlay is a window you drag or
   * type a size into. Pinning the content to `?width=` on top of that meant
   * the two fought and the fixed number always won — widening the window to
   * 700 left a 340-wide panel sitting in it, which is exactly what the size
   * control appearing to do nothing looks like.
   *
   * `?width=` is therefore no longer applied at all, rather than kept as a cap
   * — the shipped default puts `width=340` in everybody's settings, so a cap
   * would leave the control looking broken for exactly the people who never
   * chose the number. A narrow panel in a wide capture is now had by sizing
   * the source, which is where that decision already belonged. */
  app.style.width = "100%";
  if (opt.scale !== 1) app.style.transform = `scale(${opt.scale})`;

  const el = {
    meter: document.getElementById("meter"),
    title: document.getElementById("enc-title"),
    meta: document.getElementById("enc-meta"),
    rows: document.getElementById("rows"),
    sub: document.getElementById("enc-sub"),
    xpPanel: document.getElementById("xp-panel"),
    xpTitle: document.getElementById("xp-title"),
    xpMeta: document.getElementById("xp-meta"),
    xpRows: document.getElementById("xp-rows"),
    motesPanel: document.getElementById("motes-panel"),
    motesMeta: document.getElementById("motes-meta"),
    motesRows: document.getElementById("motes-rows"),
    healPanel: document.getElementById("heal-panel"),
    healRows: document.getElementById("heal-rows"),
    takenPanel: document.getElementById("taken-panel"),
    takenRows: document.getElementById("taken-rows"),
    dpsPanel: document.getElementById("dps-panel"),
    dpsMeta: document.getElementById("dps-meta"),
    dpsSpark: document.getElementById("dps-spark"),
    outgoingPanel: document.getElementById("outgoing-panel"),
    outgoingMeta: document.getElementById("outgoing-meta"),
    outgoingRows: document.getElementById("outgoing-rows"),
    incomingPanel: document.getElementById("incoming-panel"),
    incomingMeta: document.getElementById("incoming-meta"),
    incomingRows: document.getElementById("incoming-rows"),
    auraPanel: document.getElementById("aura-panel"),
    auraMeta: document.getElementById("aura-meta"),
    auras: document.getElementById("auras"),
    debuffPanel: document.getElementById("debuff-panel"),
    debuffs: document.getElementById("debuffs"),
    buffPanel: document.getElementById("buff-panel"),
    buffs: document.getElementById("buffs"),
    charmPanel: document.getElementById("charm-panel"),
    charmMeta: document.getElementById("charm-meta"),
    charmRows: document.getElementById("charm-rows"),
    appPanels: document.getElementById("app-panels"),
    skyPanel: document.getElementById("sky-panel"),
    skyMeta: document.getElementById("sky-meta"),
    skyRows: document.getElementById("sky-rows"),
    status: document.getElementById("status"),
  };

  if (!opt.panels.includes("meter")) el.meter.classList.add("hidden");
  if (!opt.panels.includes("xp")) el.xpPanel.classList.add("hidden");
  if (opt.panels.includes("motes")) el.motesPanel.classList.remove("hidden");
  if (!opt.panels.includes("heals")) el.healPanel.classList.add("hidden");
  if (!opt.panels.includes("taken")) el.takenPanel.classList.add("hidden");
  if (!opt.panels.includes("auras")) el.auraPanel.classList.add("hidden");
  if (!opt.panels.includes("debuffs")) el.debuffPanel.classList.add("hidden");
  if (!opt.panels.includes("buffs")) el.buffPanel.classList.add("hidden");

  /* Charm resists.
   *
   * Its own panel so it can live in its own window, positioned away from the
   * meter — a charm that fails is something you react to immediately, and it
   * belongs where your eyes already are, not buried in a stack of panels.
   *
   * Polled on a short interval rather than pushed: a resist matters within a
   * second or two, and the payload is tiny.
   *
   * Unlike the other panels this one stays visible while a charm is held, so
   * the uptime figure is always readable; it hides only when there is neither
   * a charm nor a recent failure to report. */
  const CHARM_POLL_MS = 1000;

  async function pollCharm() {
    if (!opt.panels.includes("charm")) return;
    try {
      const r = await fetch("/api/charm");
      if (!r.ok) return;
      const d = await r.json();
      const idle = !d.charmed.length && !d.failures.length;
      el.charmPanel.classList.toggle("hidden", idle);
      if (idle) return;

      el.charmMeta.innerHTML = d.charmed.length
        ? `<b>${(d.uptime * 100).toFixed(0)}%</b> uptime${
            d.breaks ? ` · ${d.breaks} break${d.breaks === 1 ? "" : "s"}` : ""
          }`
        : `<span class="offline">no charm</span>`;

      const held = d.charmed
        .map(
          (n) => `<div class="row pet" style="--share:100%">
            <span class="name">${esc(n)}</span>
            <span class="nums"><span class="dmg">charmed</span></span>
          </div>`
        )
        .join("");

      const fails = d.failures
        .slice(0, opt.rows)
        .map((f) => {
          // Log times are the game's local wall clock written as if it
          // were UTC, so "now" has to be put on that clock too, or every
          // failure is hours old west of Greenwich.
          const nowLog = Date.now() / 1000 - new Date().getTimezoneOffset() * 60;
          const ago = Math.max(0, Math.round(nowLog - f.at));
          return `<div class="row enemy" style="--share:100%">
            <span class="name">${esc(f.target)}
              <span class="tag fail">${esc(f.kind)}</span></span>
            <span class="nums"><span class="dmg">${ago}s ago</span></span>
          </div>`;
        })
        .join("");

      el.charmRows.innerHTML = held + fails;
    } catch (e) {
      /* daemon busy; try again next tick */
    }
  }

  if (opt.panels.includes("charm")) {
    pollCharm();
    setInterval(pollCharm, CHARM_POLL_MS);
  }

  /* Damage as it happens, rather than averaged over the fight.
   *
   * The meter's dps figure is the fight's total over its length, which is the
   * right number for comparing pulls and the wrong one for seeing what you are
   * doing now: it barely moves after the first thirty seconds, so a burn and a
   * lull look identical while you are in them.
   *
   * Derived here rather than served: the snapshot already carries a running
   * damage total and the log's own clock, and the difference between two of
   * them is the answer. Nothing needs to be added to a payload that every
   * client pays for, and no state has to be kept in the daemon.
   *
   * Timed on the log clock, not the wall clock. A parse catching up after a
   * pause delivers many snapshots in one instant, and wall-clock timing would
   * read that backlog as an enormous burst.
   */
  const DPS_ROLL_SECONDS = 10;
  const DPS_SPARK_SECONDS = 60;
  /** damage dealt in each log-second, keyed by that second. */
  const dpsPerSecond = new Map();
  let dpsPrev = null;
  let dpsFight = null;

  function renderDps(snap, enc) {
    if (!opt.panels.includes("dps")) return;
    const now = snap.log_time;
    if (!enc || !now) {
      el.dpsPanel.classList.add("hidden");
      return;
    }
    // A new fight starts a new history; carrying the last one over would show
    // a burst that belongs to a creature already dead.
    if (enc.id !== dpsFight) {
      dpsFight = enc.id;
      dpsPerSecond.clear();
      dpsPrev = null;
    }

    const total = enc.your_damage || 0;
    if (dpsPrev && total > dpsPrev.total) {
      // Attribute the gain to the second it was reported in. A single log
      // second can carry several snapshots; they accumulate into one bucket.
      dpsPerSecond.set(now, (dpsPerSecond.get(now) || 0) + (total - dpsPrev.total));
    }
    dpsPrev = { at: now, total };
    for (const t of dpsPerSecond.keys()) {
      if (now - t > DPS_SPARK_SECONDS) dpsPerSecond.delete(t);
    }

    // Seconds with no damage are real zeroes, not gaps: the log only writes a
    // line when something happens, so an idle second is simply absent and has
    // to be filled in or the chart would compress the lulls out of existence.
    const buckets = [];
    for (let t = now - DPS_SPARK_SECONDS + 1; t <= now; t++) {
      buckets.push(dpsPerSecond.get(t) || 0);
    }
    const rolled = buckets.slice(-DPS_ROLL_SECONDS).reduce((a, b) => a + b, 0);
    // Divided by the seconds the window actually covers, not by its size. A
    // fight three seconds old has seven empty buckets padding the window, and
    // dividing by ten reported a third of the true rate for the opening of
    // every pull — precisely where the burst is.
    const covered = Math.max(1, Math.min(DPS_ROLL_SECONDS, enc.duration || DPS_ROLL_SECONDS));
    const rolling = Math.round(rolled / covered);

    const peak = Math.max(1, ...buckets);
    el.dpsPanel.classList.remove("hidden");
    // Peak belongs on the header line with the other two figures. Floated over
    // the chart it was unreadable the moment a tall bar reached the right-hand
    // edge — which is precisely when the peak is worth reading.
    el.dpsMeta.innerHTML =
      `<b>${num(rolling)}</b> now &middot; ${num(enc.your_dps)} avg &middot; peak ${num(peak)}`;

    const w = 100 / buckets.length;
    // preserveAspectRatio is off on purpose here, unlike the fight timeline:
    // these are solid bars at a fixed count, so stretching them horizontally
    // widens each bar rather than piling separate events on top of each other.
    el.dpsSpark.innerHTML = `<svg viewBox="0 0 100 24" preserveAspectRatio="none">${buckets
      .map((v, i) =>
        v
          ? `<rect x="${(i * w).toFixed(2)}" y="${(24 - (v / peak) * 24).toFixed(2)}"
               width="${(w * 0.85).toFixed(2)}" height="${((v / peak) * 24).toFixed(2)}"></rect>`
          : ""
      )
      .join("")}</svg>`;
  }

  /* Your own output, broken down the way "Hitting you" breaks down theirs.
   *
   * The meter says how much you did and how you rank; it never says what you
   * did it with. Which spell is actually carrying a fight is a question you
   * can only answer afterwards on the encounter page, by which time the answer
   * has stopped being actionable.
   *
   * Read straight off the snapshot, which already carries the breakdown for
   * the encounter page — no request of its own, and it updates at the same
   * rate as everything else on the overlay.
   *
   * Abilities are pooled across their groups and ranked together, and the bar
   * is a share of your whole output rather than of its group. The groups are
   * scaled to themselves, so a melee swing at 76% of your melee would draw a
   * longer bar than a nuke at 40% of your total.
   */
  function renderOutgoing(enc) {
    if (!opt.panels.includes("outgoing")) return;
    const groups = (enc && enc.outgoing_groups) || [];
    const total = (enc && enc.your_damage) || 0;
    if (!groups.length || !total) {
      el.outgoingPanel.classList.add("hidden");
      return;
    }
    const abilities = groups
      .flatMap((g) => (g.abilities || []).map((a) => ({ ...a, group: g.group })))
      .sort((a, b) => b.damage - a.damage)
      .slice(0, opt.rows);

    el.outgoingPanel.classList.remove("hidden");
    el.outgoingMeta.innerHTML = `<b>${num(total)}</b> dealt &middot; ${num(enc.your_dps)} dps`;
    el.outgoingRows.innerHTML = abilities
      .map(
        (a) => `<div class="row you" style="--share:${((a.damage / total) * 100).toFixed(1)}%">
          <span class="name">${esc(a.name)}</span>
          <span class="nums">
            <span class="dim">${num(a.hits)}× max ${num(a.max)}</span>
            <span class="dmg">${num(a.damage)}</span>
          </span></div>`
      )
      .join("");
  }

  /* What is landing on you in the fight you are in.
   *
   * The "Damage taken" panel above answers a different question — it ranks the
   * creatures in the fight by how much damage each one absorbed. Neither it nor
   * the meter ever names the thing hitting *you*, so a boss AE for 472 every
   * thirty seconds shows up only as a total quietly climbing.
   *
   * Scoped to the current encounter rather than the log: the report defaults to
   * every fight ever, which is a different question and a useless one mid-pull.
   * Nothing to scope it to means nothing to show, so the panel hides between
   * fights rather than displaying a stale fight's numbers.
   *
   * Slower than the charm poll: charm breaks are reacted to within a second,
   * while this is a picture of a fight and changes on the timescale of a boss
   * casting. */
  const INCOMING_POLL_MS = 2000;

  async function pollIncoming() {
    if (!opt.panels.includes("incoming")) return;
    const enc = lastSnapshot && lastSnapshot.current;
    if (!enc) {
      el.incomingPanel.classList.add("hidden");
      return;
    }
    try {
      const r = await fetch(`/api/incoming?encounter=${enc.id}`);
      if (!r.ok) return;
      const d = await r.json();
      el.incomingPanel.classList.toggle("hidden", !d.total);
      if (!d.total) return;

      el.incomingMeta.innerHTML = `<b>${num(d.total)}</b> taken${
        d.biggest_hit ? ` · worst ${num(d.biggest_hit)}` : ""
      }`;
      el.incomingRows.innerHTML = (d.by_ability || [])
        .slice(0, opt.rows)
        .map(
          (a) => `<div class="row enemy" style="--share:${(a.share * 100).toFixed(1)}%">
            <span class="name">${esc(a.name)}</span>
            <span class="nums">
              <span class="dim">${num(a.hits)}× max ${num(a.max)}</span>
              <span class="dmg">${num(a.total)}</span>
            </span></div>`
        )
        .join("");
    } catch (e) {
      /* daemon busy; try again next tick */
    }
  }

  // Interval only, with no call on the way past: `lastSnapshot` is declared
  // further down the file, so running this during module evaluation would hit
  // its temporal dead zone and take the whole overlay down with it. There is
  // also nothing to report until a snapshot has arrived.
  if (opt.panels.includes("incoming")) {
    setInterval(pollIncoming, INCOMING_POLL_MS);
  }

  /* Sky turn-ins.
   *
   * Polled rather than pushed: the snapshot goes out on every log line, and a
   * quest list that changes a few times an hour has no business riding along
   * with it. Twenty seconds is well inside the time it takes to walk to an
   * island.
   *
   * The panel hides itself when nothing is ready, so it costs no screen space
   * on the overwhelming majority of frames — the same rule the XP potion line
   * follows. */
  const SKY_POLL_MS = 20000;

  async function pollSky() {
    if (!opt.panels.includes("sky")) return;
    try {
      const r = await fetch("/api/sky");
      if (!r.ok) return;
      const d = await r.json();
      const ready = d.quests.filter((q) => q.relevant && q.complete);
      el.skyPanel.classList.toggle("hidden", ready.length === 0);
      if (!ready.length) return;
      el.skyMeta.textContent = `${ready.length} of ${d.relevant_count}`;
      el.skyRows.innerHTML = ready
        .slice(0, opt.rows)
        .map(
          (q) => `<div class="row you" style="--share:100%">
            <span class="name">${esc(q.name)}</span>
            <span class="nums"><span class="dmg">${esc(q.giver)}</span></span>
          </div>
          <div class="row"><span class="name reward">→ ${esc(q.reward)}</span>
            <span class="nums"><span class="dmg">say "${esc(q.phrase)}"</span></span>
          </div>`
        )
        .join("");
    } catch (e) {
      /* daemon busy or restarting; try again next tick */
    }
  }

  if (opt.panels.includes("sky")) {
    pollSky();
    setInterval(pollSky, SKY_POLL_MS);
  }

  /* Panels other apps put up: the overlay's extension slot.

     The eqldb collector prompts "worth a picture" here, which is why
     `wanted` turns these on as well as `apps`: an overlay URL saved with
     `wanted` in it, from when the daemon drew that prompt itself, keeps
     showing it.

     The daemon only stores these. Each lapses unless its app puts it again,
     so a closed app's panel goes on its own. */
  const APP_POLL_MS = 5000;

  async function pollAppPanels() {
    try {
      const r = await fetch("/api/panels");
      if (!r.ok) return;
      const d = await r.json();
      const panels = (d.panels || []).filter((p) => (p.rows || []).length);
      el.appPanels.innerHTML = panels
        .map(
          (p) => `<div class="panel">
            <div class="head">
              <span class="title">${esc(p.title)}</span>
              <span class="meta">${esc(p.meta || "")}</span>
            </div>
            <div class="rows">${(p.rows || [])
              .map(
                (row) => `<div class="row ${row.highlight ? "enemy" : "you"}" style="--share:100%">
                  <span class="name">${esc(row.name)}</span>
                  <span class="nums"><span class="dmg">${esc(row.value || "")}</span></span>
                </div>${
                  row.note || row.note_value
                    ? `<div class="row"><span class="name reward">${esc(row.note || "")}</span>
                        <span class="nums"><span class="dmg">${esc(row.note_value || "")}</span></span>
                      </div>`
                    : ""
                }`
              )
              .join("")}</div>
          </div>`
        )
        .join("");
    } catch (e) {
      /* daemon busy or restarting; the last answer stands until next tick */
    }
  }

  if (opt.panels.includes("apps") || opt.panels.includes("wanted")) {
    pollAppPanels();
    setInterval(pollAppPanels, APP_POLL_MS);
  }

  /* Session totals.
   *
   * Polled, not pushed: merging every fight of a session is too much work to
   * do on each snapshot, which the WebSocket sends on every log line. Two
   * seconds is imperceptible on a figure covering an evening.
   *
   * Rates from this endpoint are per second *of combat* — the daemon divides
   * by time actually spent fighting, not by wall clock, so the number means
   * the same thing as the one on a single pull. */
  const SESSION_POLL_MS = 2000;
  let sessionSummary = null;
  let sessionMeta = null;

  async function pollSession() {
    if (!opt.scope) return;
    try {
      const r = await fetch(`/api/session?scope=${opt.scope}`);
      if (!r.ok) return;
      const d = await r.json();
      sessionSummary = d.fights ? d.summary : null;
      sessionMeta = d;
      if (lastSnapshot) render(lastSnapshot);
    } catch (e) {
      /* daemon busy; try again next tick */
    }
  }

  if (opt.scope) {
    pollSession();
    setInterval(pollSession, SESSION_POLL_MS);
  }

  // ---- formatting -------------------------------------------------------

  /* These two must match `num` and `dur` in gui/renderer/app.js exactly.
   *
   * They are duplicated rather than shared because this page is served by the
   * daemon and that one is loaded from disk by Electron, with no build step
   * between them and a content-security policy that forbids the window
   * fetching a script from the daemon. So the only thing keeping them in step
   * is this note.
   *
   * They had already drifted, and it showed: under `?scope=session` the
   * meter—s duration is an evening of combat, and this copy had no hours
   * branch, so 4316 seconds read as "71:56" here against "1:11:56" in the
   * app — one figure from one endpoint, formatted two ways. This copy also
   * turned a missing number into "0" rather than a dash, which is the one
   * thing a parser that reports floors must never do.
   */

  /** Compact large damage numbers; overlays are narrow. */
  function num(n) {
    if (n == null) return "—";
    if (n >= 1e9) return (n / 1e9).toFixed(2) + "B";
    if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
    if (n >= 1e4) return (n / 1e3).toFixed(1) + "k";
    return Math.round(n).toLocaleString();
  }

  function dur(sec) {
    sec = Math.max(0, Math.round(sec || 0));
    const h = Math.floor(sec / 3600);
    const m = Math.floor((sec % 3600) / 60);
    const s = sec % 60;
    if (h) return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
    if (m) return `${m}:${String(s).padStart(2, "0")}`;
    return `${s}s`;
  }

  /** Escape text before it reaches innerHTML: mob and player names are
   *  attacker-controlled in the sense that they come from the game, and a
   *  stray angle bracket should never be able to alter the DOM. */
  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
    })[c]);
  }

  // ---- rendering --------------------------------------------------------

  let lastSnapshot = null;

  function render(snap) {
    lastSnapshot = snap;
    // With a scope set, the meter is the running total rather than the pull.
    // Everything else on the page — buffs, auras, xp — is still about now.
    const enc = opt.scope
      ? sessionSummary
      : snap.current || (opt.keepIdle ? snap.recent && snap.recent[0] : null);

    if (!enc) {
      el.title.textContent = opt.scope ? "Nothing fought yet" : "Waiting for combat";
      el.title.classList.add("idle");
      el.meta.textContent = "";
      el.rows.innerHTML = '<div class="empty">No active encounter</div>';
      el.healRows.innerHTML = '<div class="empty">—</div>';
      el.takenRows.innerHTML = '<div class="empty">—</div>';
      el.sub.innerHTML = "";
    } else {
      el.title.textContent = enc.title || "Encounter";
      // A session total is never "idle": it is not waiting on anything.
      el.title.classList.toggle("idle", !opt.scope && !snap.current);
      // For a scoped meter the duration is combat time, so say what it counts
      // rather than let it read as how long you have been playing.
      el.meta.innerHTML = opt.scope
        ? `<b>${num(enc.your_dps)}</b> dps &middot; ${dur(enc.duration)} fighting &middot; ${
            sessionMeta ? sessionMeta.fights : 0
          } fights`
        : `<b>${num(enc.your_dps)}</b> dps &middot; ${dur(enc.duration)}`;
      renderRows(enc);
      renderMetric(el.healRows, enc, "healing", "hps", "heal_share", "No healing yet");
      renderMetric(el.takenRows, enc, "damage_taken", null, "taken_share", "No damage taken");
      renderSub(snap, enc);
    }
    // Enough panels will outgrow any window — the desktop overlay fits itself
    // to its content but cannot exceed the screen, and an OBS source is
    // whatever size it was made. Clipping is then unavoidable; clipping
    // *silently* is not, and a panel that is simply missing reads as a bug.
    markOverflow();

    // Outside the branch, like the trace below: motes accrue whether or not
    // anything is being fought, and a panel that blanks between pulls would
    // be reporting the fight rather than the session.
    if (opt.panels.includes("motes")) renderMotes(snap);

    // Outside the branch: it has to see an idle tick to keep the trace moving
    // when you stop swinging, and to clear itself when the fight ends.
    renderDps(snap, opt.scope ? null : snap.current);
    // `enc` rather than snap.current, so a scoped overlay breaks down the
    // session or zone it is scoped to instead of contradicting its own meter.
    renderOutgoing(enc);

    renderXp(snap);
    renderAuras(snap.auras || []);
    renderDebuffs(snap.debuffs || []);
    renderBuffs(snap.buffs || []);
    // `live: false` is the daemon still reading the log's backlog, not a lost
    // connection: say so, since a long log can take a minute.
    if (snap.live === false) el.status.textContent = "Reading the log…";
    el.status.classList.toggle("hidden", snap.live !== false);
  }

  /* A creature's pet folded into the creature.
   *
   * It exists because its owner summoned it and dies when the owner dies, so
   * it is not a combatant in its own right — it is part of what that creature
   * is doing to you. Two rows for one thing is expensive on an overlay, where
   * the row budget is small and every line spent on "King Tranix pet" is a
   * line not spent on something you did not already know.
   *
   * Before the row limit, not after, or the fold would free a slot and then
   * not fill it. Your own pets are left alone: the split between your damage
   * and your pets' is one the meter deliberately keeps.
   *
   * Copies, because the snapshot object is re-rendered on every tick and
   * adding into a live row would compound it. */
  function foldPets(rows) {
    const out = rows.map((r) => ({ ...r }));
    const byName = new Map(out.map((r) => [r.name, r]));
    const absorbed = new Set();
    for (const r of out) {
      const owner = r.pet_owner && byName.get(r.pet_owner);
      // Summoned only — the naming convention or an answer to `/pet leader`.
      // A charmed mob is its own creature that changed sides and can change
      // back, so its damage is not its captor's to absorb, and a pet of
      // unknown kind may be a charm nothing managed to classify.
      if (!owner || r.is_you || r.is_pet || owner === r) continue;
      if (r.pet_kind !== "summoned") continue;
      owner.damage += r.damage;
      owner.dps += r.dps;
      owner.share += r.share;
      owner.foldedPets = (owner.foldedPets || 0) + 1;
      absorbed.add(r);
    }
    return out.filter((r) => !absorbed.has(r));
  }

  /* How long a row stays at full strength, and how it fades after that.

     Eight seconds because a fight has lulls — a caster between spells, a mob
     mid-swing — and blinking at every one of them would make the meter
     restless. By half a minute of nothing it is down to a third, which reads
     as "was here" without pretending to be gone. */
  const STALE_AFTER = 8;
  const STALE_FADE = 22;
  const STALE_FLOOR = 0.34;

  /** Flag the page when it is taller than the space it has. */
  function markOverflow() {
    const app = document.getElementById("app");
    if (!app) return;
    const over = Math.ceil(app.getBoundingClientRect().height) - window.innerHeight > 2;
    document.body.classList.toggle("clipped", over);
  }
  window.addEventListener("resize", markOverflow);

  /* Motes looted this sitting, cheapest tier first.

     A bar per tier against the biggest, which makes the shape of a night
     legible at a glance — a run of Infinitesimals reads differently from two
     Majors even when the totals match. */
  function renderMotes(snap) {
    const rows = snap.session_motes || [];
    const total = snap.session_motes_total || 0;
    // A rate as well as a count: on a farming night the question is whether
    // this hour is better than the last one, which a running total cannot
    // answer. Held back until the sitting is long enough to mean something —
    // three motes in the first four minutes is not 45 an hour.
    const hours = (snap.session_seconds || 0) / 3600;
    const rate = total && hours >= 0.25 ? ` · ${(total / hours).toFixed(1)}/hr` : "";
    el.motesMeta.textContent = total ? `${total} this session${rate}` : "";
    if (!rows.length) {
      el.motesRows.innerHTML = '<div class="empty">None yet</div>';
      return;
    }
    const max = Math.max(...rows.map(([, n]) => n), 1);
    el.motesRows.innerHTML = rows
      .map(
        ([name, n]) => `<div class="row" style="--share:${((n / max) * 100).toFixed(1)}%">
          <span class="name">${esc(name.replace(/^Mote of /i, ""))}</span>
          <span class="nums"><span class="dmg">${n}</span></span>
        </div>`
      )
      .join("");
  }

  function renderRows(enc) {
    let meter = foldPets(enc.meter || []);
    if (opt.selfOnly) meter = meter.filter((r) => r.is_you);
    // A charmed pet is an enemy by origin but an ally in effect; keep it even
    // when enemies are hidden, since its damage is credited to the player.
    if (!opt.showEnemies) meter = meter.filter((r) => !r.is_enemy || r.is_pet);
    meter = meter.slice(0, opt.rows);

    if (meter.length === 0) {
      el.rows.innerHTML = '<div class="empty">No damage yet</div>';
      return;
    }

    el.rows.innerHTML = meter
      .map((r) => {
        const cls = ["row"];
        if (r.is_you) cls.push("you");
        else if (r.is_pet) cls.push("pet");
        else if (r.is_enemy) cls.push("enemy");
        // A charm can break mid-fight and hand its damage back to the other
        // side; a summoned pet cannot. Worth two words on a narrow overlay.
        // Another player's pet is labelled with whose it is.
        // A row carrying a folded pet says so, or its total quietly disagrees
        // with the log.
        const tag = r.foldedPets
          ? `<span class="tag other">+${r.foldedPets} pet${r.foldedPets === 1 ? "" : "s"}</span>`
          : r.pet_kind
            ? `<span class="tag${r.is_pet ? "" : " other"}">${
                r.pet_kind === "charmed" ? "charmed" : "pet"
              }${!r.is_pet && r.pet_owner ? " " + esc(r.pet_owner) : ""}</span>`
            : "";
        // Fade what has gone quiet. A fight is one encounter until combat
        // stops, so a mob killed in the first ten seconds of a three-minute
        // pull keeps its row for the rest of it. That is right for the totals
        // and wrong on a live meter, where a name sitting at full strength
        // reads as something still standing there.
        //
        // Faded rather than dropped: its damage is still part of the fight,
        // and a row vanishing mid-pull is worse than one going quiet. Measured
        // against the encounter's own end, which advances with the log, so no
        // wall clock is involved.
        const quiet = r.last_action ? Math.max(0, (enc.end || 0) - r.last_action) : 0;
        const fade =
          quiet <= STALE_AFTER
            ? 1
            : Math.max(STALE_FLOOR, 1 - (quiet - STALE_AFTER) / STALE_FADE);
        return `<div class="${cls.join(" ")}" style="--share:${(r.share * 100).toFixed(1)}%${
          fade < 1 ? `;opacity:${fade.toFixed(2)}` : ""
        }">
          <span class="name">${esc(r.name)}${tag}</span>
          <span class="nums">
            ${critCell(r)}
            <span class="dmg">${num(r.damage)}</span>
            <span class="dps">${num(r.dps)}</span>
          </span>
        </div>`;
      })
      .join("");
  }

  /* Crit rate for a meter row. The overall figure covers every mechanism at
     once — shields never crit and dot ticks crit at their own rate — so it is
     a summary, not a stat to tune against; the desktop app breaks it down by
     type. Rendered as a column even at 0% so the numbers stay aligned, and
     dropped entirely with ?crit=0 for narrow overlays. */
  function critCell(r) {
    if (!opt.showCrit) return "";
    return `<span class="crit">${((r.crit_rate || 0) * 100).toFixed(0)}%</span>`;
  }

  /* Render a meter for a metric other than damage (healing, damage taken).
     The server sends one row set with a precomputed share per metric, so each
     panel only has to filter, sort and pick the right fields. */
  function renderMetric(target, enc, valueKey, rateKey, shareKey, emptyText) {
    const rows = (enc.meter || [])
      .filter((r) => r[valueKey] > 0)
      .sort((a, b) => b[valueKey] - a[valueKey])
      .slice(0, opt.rows);

    if (rows.length === 0) {
      target.innerHTML = `<div class="empty">${emptyText}</div>`;
      return;
    }
    target.innerHTML = rows
      .map((r) => {
        const cls = ["row"];
        if (r.is_you) cls.push("you");
        else if (r.is_pet) cls.push("pet");
        else if (r.is_enemy) cls.push("enemy");
        const rate = rateKey ? `<span class="dps">${num(r[rateKey])}</span>` : "";
        return `<div class="${cls.join(" ")}" style="--share:${(r[shareKey] * 100).toFixed(1)}%">
          <span class="name">${esc(r.name)}</span>
          <span class="nums">
            <span class="dmg">${num(r[valueKey])}</span>
            ${rate}
          </span>
        </div>`;
      })
      .join("");
  }

  function renderSub(snap, enc) {
    const bits = [];
    if (snap.zone) bits.push(`<span class="chip">${esc(snap.zone)}</span>`);
    // "to enemy" rather than "total": the raw total counts the mob hitting
    // you, which reads as inflated damage output.
    if (enc.damage_to_enemies)
      bits.push(`<span class="chip">to enemy ${num(enc.damage_to_enemies)}</span>`);
    if (enc.your_hps > 1) bits.push(`<span class="chip">${num(enc.your_hps)} hps</span>`);
    if (enc.your_damage_taken)
      bits.push(`<span class="chip">taken ${num(enc.your_damage_taken)}</span>`);
    if (enc.your_deaths) bits.push(`<span class="chip">deaths ${enc.your_deaths}</span>`);
    el.sub.innerHTML = bits.join("");
  }

  /* Progress through the current level, with the rate and a projected time to
     level. The bar is the headline; the numbers are secondary.

     At the cap the bar disappears and the panel becomes an AA readout, which
     is the only thing still moving. */
  /* A span that may be long.

     `dur` counts every second, which is right for a fight and absurd for an
     estimate that can run to days — a quiet hour would put "16:40:00" on
     the overlay, claiming a precision the rate behind it does not have.
     Above an hour this coarsens to hours and days instead. */
  function longDur(sec, coarse) {
    sec = Math.max(0, Math.round(sec));
    // An estimate of a whole level does not want seconds: "21m" is the claim
    // being made, and "21:15" implies a precision the rate does not have.
    if (sec < 3600) return coarse ? `${Math.max(1, Math.round(sec / 60))}m` : dur(sec);
    const h = Math.floor(sec / 3600);
    if (h < 24) {
      const m = Math.round((sec % 3600) / 60);
      return m ? `${h}h ${m}m` : `${h}h`;
    }
    const d = Math.floor(h / 24);
    const r = h % 24;
    return r ? `${d}d ${r}h` : `${d}d`;
  }

  function renderXp(snap) {
    const x = snap.exp;
    if (!x) return;
    // The effective level and the class earning it. Classes level separately,
    // so "level 13" on a loadout whose lowest class is 28 is the berserker
    // that was put away hours ago.
    const lvl = snap.level || 0;
    el.xpTitle.textContent = snap.levelling
      ? `Level ${lvl} (${snap.levelling})`
      : `Level ${lvl}`;
    // At the cap there is no next level, so the bar, the projection and the
    // %/hr rate all describe something that cannot happen. `snap.level` is the
    // *lowest* class in the loadout, so this only fires once every class is
    // capped — while anything can still gain a level, the bar stays.
    // The game's cap, from the daemon: 50 in Legends, 130 in live EverQuest.
    // An older daemon does not send it, and only ever read Legends.
    const capped = lvl >= (snap.level_cap || 50);
    const bits = [];
    if (!capped && x.rate_per_hour > 0)
      bits.push(`<b>${x.rate_per_hour.toFixed(1)}</b> %/hr`);
    if (x.aa_per_hour > 0) bits.push(`<b>${x.aa_per_hour.toFixed(1)}</b> AA/hr`);
    // What a whole level costs at this rate, which is the planning number —
    // "how long until the next one" answers tonight, "how long a level takes"
    // answers whether to start one. Derived rather than sent: it is the same
    // rate the line already carries, turned over.
    if (!capped && x.rate_per_hour > 0)
      bits.push(`<b>${longDur((100 / x.rate_per_hour) * 3600, true)}</b>/level`);
    el.xpMeta.innerHTML = bits.join(" &middot; ");
    const pct = Math.max(0, Math.min(100, x.percent));
    const eta =
      x.seconds_to_next_level != null ? longDur(x.seconds_to_next_level) : "—";
    const levelRow = capped
      ? ""
      : `<div class="row you" style="--share:${pct.toFixed(1)}%">
      <span class="name">${pct.toFixed(1)}% &rarr; ${lvl + 1}</span>
      <span class="nums">
        <span class="dmg">${x.kills} kills</span>
        <span class="dps">${eta}</span>
      </span></div>`;
    el.xpRows.innerHTML = `${levelRow}
      <!-- Unspent only. The client states the balance outright, so it is
           always right; lifetime spent has to be reconstructed from purchases
           in the log plus whatever baseline the player typed in, and in
           practice it is wrong more often than not. A number nobody can trust
           is worse on an overlay than no number at all. -->
      <div class="row" style="--share:0%">
        <span class="name">AA</span>
        <span class="nums">
          <span class="dps">${x.aa_unspent} unspent</span>
        </span></div>
      ${potionRow(x)}`;
  }

  /* Potion charges remaining. Hidden entirely when no potion is up, so the
     overlay stays quiet during the long stretches without one — charges are
     the only expiry signal the client gives, since it logs no potion timer.
     The row turns amber on the last charge as a cue to drink another. */
  function potionRow(x) {
    if (!x.aa_potion_active) return "";
    const c = x.aa_potion_charges;
    return `<div class="row ${c <= 1 ? "" : "pet"}" style="--share:${(Math.min(c, 5) / 5) * 100}%">
      <span class="name">XP potion</span>
      <span class="nums"><span class="dps">${c} left</span></span>
    </div>`;
  }

  /* Auras have no meaningful countdown; only whether they are up matters.
     Bards run five slots, so the count is shown against that. */
  function renderAuras(auras) {
    el.auraMeta.textContent = auras.length ? `${auras.length}/5` : "";
    if (!auras.length) {
      el.auras.innerHTML = '<div class="empty">No auras</div>';
      return;
    }
    el.auras.innerHTML = auras
      .slice(0, 5)
      .map(
        (a) => `<div class="row pet" style="--share:100%">
        <span class="name">${esc(a.name)}</span>
        <span class="nums"><span class="dps">up</span></span>
      </div>`
      )
      .join("");
  }

  /* Detrimental effects. Coloured as threats rather than timers: what matters
     is that one is on you, and how long until it drops. */
  function renderDebuffs(debuffs) {
    const list = debuffs.filter((d) => d.remaining != null).slice(0, opt.buffs);
    if (!list.length) {
      el.debuffs.innerHTML = '<div class="empty">None</div>';
      return;
    }
    el.debuffs.innerHTML = list
      .map((d) => {
        const total = d.total || 1;
        const pct = d.permanent ? 100 : Math.max(0, Math.min(100, (d.remaining / total) * 100));
        return `<div class="buff critical" style="--remain:${pct.toFixed(1)}%">
          <span class="bname">${esc(d.name)}</span>
          <span class="btime">${d.permanent ? "&infin;" : dur(d.remaining)}</span>
        </div>`;
      })
      .join("");
  }

  function renderBuffs(buffs) {
    /* Until-you-zone effects would otherwise dominate the list with
       multi-day countdowns, pushing the timers that actually need watching
       off the bottom. Real timers first, permanents after. */
    const list = buffs
      .filter((b) => b.remaining != null)
      .sort((a, b) => (a.permanent ? 1 : 0) - (b.permanent ? 1 : 0))
      .slice(0, opt.buffs);

    if (list.length === 0) {
      el.buffs.innerHTML = '<div class="empty">No tracked buffs</div>';
      return;
    }
    el.buffs.innerHTML = list
      .map((b) => {
        const total = b.total || 1;
        // Past the time the spell file gives it, and still taken to be on
        // (live EverQuest extends buffs by an amount the log never states):
        // how long past, rather than a timer stuck at zero.
        const over = b.overrun != null;
        const pct = b.permanent
          ? 100
          : over
            ? 0
            : Math.max(0, Math.min(100, (b.remaining / total) * 100));
        const cls = ["buff", `cat-${(b.category || "other").replace(/[^a-z]/g, "")}`];
        if (over) cls.push("overrun");
        else if (!b.permanent) {
          if (b.remaining <= 10) cls.push("urgent");
          else if (b.remaining <= 30) cls.push("expiring");
        }
        // Landing text is ambiguous for many spells; flag low-confidence
        // identifications rather than presenting a guess as fact.
        if (b.confidence < 0.8) cls.push("unsure");
        return `<div class="${cls.join(" ")}" style="--remain:${pct.toFixed(1)}%">
          <span class="bname">${esc(b.name)}</span>
          <span class="btime">${
            b.permanent ? "&infin;" : over ? `+${longDur(b.overrun, true)}` : dur(b.remaining)
          }</span>
        </div>`;
      })
      .join("");
  }

  // ---- connection -------------------------------------------------------

  let ws = null;
  let retry = 0;

  function connect() {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    ws = new WebSocket(`${proto}//${location.host}/ws`);

    ws.onopen = () => {
      retry = 0;
      el.status.classList.add("hidden");
    };

    ws.onmessage = (ev) => {
      try {
        render(JSON.parse(ev.data));
      } catch (e) {
        console.error("bad snapshot", e);
      }
    };

    ws.onclose = () => {
      el.status.classList.remove("hidden");
      el.status.textContent = "Reconnecting…";
      // Back off, but stay responsive: an overlay that gives up is useless
      // when the daemon restarts mid-session.
      retry = Math.min(retry + 1, 10);
      setTimeout(connect, Math.min(500 * retry, 5000));
    };

    ws.onerror = () => ws.close();
  }

  connect();

  /* Buff timers must keep counting between server pushes, otherwise they
     visibly stutter at the push interval. Re-render locally each second,
     decrementing from the last known snapshot. */
  setInterval(() => {
    if (!lastSnapshot || !lastSnapshot.buffs) return;
    if (lastSnapshot.debuffs) {
      lastSnapshot.debuffs = lastSnapshot.debuffs.map((b) =>
        b.remaining == null || b.permanent
          ? b
          : { ...b, remaining: Math.max(0, b.remaining - 1) }
      );
      renderDebuffs(lastSnapshot.debuffs);
    }
    lastSnapshot.buffs = lastSnapshot.buffs.map((b) =>
      b.remaining == null || b.permanent
        ? b
        : b.overrun != null
          ? { ...b, overrun: b.overrun + 1 }
          : { ...b, remaining: Math.max(0, b.remaining - 1) }
    );
    renderBuffs(lastSnapshot.buffs);
  }, 1000);
})();
