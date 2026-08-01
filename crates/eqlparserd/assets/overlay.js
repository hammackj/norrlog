/* Overlay client.
 *
 * Connects to the daemon's WebSocket and renders the live snapshot. Every
 * behaviour is configurable by query string so the same page can serve an OBS
 * Browser Source, a small always-on-top desktop window, and a debugging view:
 *
 *   ?panels=meter,buffs   panels: meter, xp, heals, taken, auras, buffs, debuffs
 *   ?rows=6               max meter rows (applies to each meter panel)
 *   ?buffs=8              max buff timers
 *   ?scale=1.25           zoom, for high-resolution captures
 *   ?width=340            overlay width in px
 *   ?self=1               show only your own row in the meter
 *   ?enemies=0            hide enemy rows
 *   ?idle=1               keep the last fight on screen when combat ends
 */

(() => {
  "use strict";

  const params = new URLSearchParams(location.search);
  const opt = {
    panels: (params.get("panels") || "meter,buffs").split(",").map((s) => s.trim()),
    rows: int(params.get("rows"), 6),
    buffs: int(params.get("buffs"), 8),
    scale: float(params.get("scale"), 1),
    width: int(params.get("width"), 340),
    selfOnly: params.get("self") === "1",
    showEnemies: params.get("enemies") !== "0",
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
  app.style.width = opt.width + "px";
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
    healPanel: document.getElementById("heal-panel"),
    healRows: document.getElementById("heal-rows"),
    takenPanel: document.getElementById("taken-panel"),
    takenRows: document.getElementById("taken-rows"),
    auraPanel: document.getElementById("aura-panel"),
    auraMeta: document.getElementById("aura-meta"),
    auras: document.getElementById("auras"),
    debuffPanel: document.getElementById("debuff-panel"),
    debuffs: document.getElementById("debuffs"),
    buffPanel: document.getElementById("buff-panel"),
    buffs: document.getElementById("buffs"),
    skyPanel: document.getElementById("sky-panel"),
    skyMeta: document.getElementById("sky-meta"),
    skyRows: document.getElementById("sky-rows"),
    status: document.getElementById("status"),
  };

  if (!opt.panels.includes("meter")) el.meter.classList.add("hidden");
  if (!opt.panels.includes("xp")) el.xpPanel.classList.add("hidden");
  if (!opt.panels.includes("heals")) el.healPanel.classList.add("hidden");
  if (!opt.panels.includes("taken")) el.takenPanel.classList.add("hidden");
  if (!opt.panels.includes("auras")) el.auraPanel.classList.add("hidden");
  if (!opt.panels.includes("debuffs")) el.debuffPanel.classList.add("hidden");
  if (!opt.panels.includes("buffs")) el.buffPanel.classList.add("hidden");

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

  // ---- formatting -------------------------------------------------------

  /** Compact large damage numbers; overlays are narrow. */
  function num(n) {
    if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
    if (n >= 1e4) return (n / 1e3).toFixed(1) + "k";
    return String(Math.round(n));
  }

  function dur(sec) {
    sec = Math.max(0, Math.round(sec));
    const m = Math.floor(sec / 60);
    const s = sec % 60;
    return m > 0 ? `${m}:${String(s).padStart(2, "0")}` : `${s}s`;
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
    const enc = snap.current || (opt.keepIdle ? snap.recent && snap.recent[0] : null);

    if (!enc) {
      el.title.textContent = "Waiting for combat";
      el.title.classList.add("idle");
      el.meta.textContent = "";
      el.rows.innerHTML = '<div class="empty">No active encounter</div>';
      el.healRows.innerHTML = '<div class="empty">—</div>';
      el.takenRows.innerHTML = '<div class="empty">—</div>';
      el.sub.innerHTML = "";
    } else {
      el.title.textContent = enc.title || "Encounter";
      el.title.classList.toggle("idle", !snap.current);
      el.meta.innerHTML = `<b>${num(enc.your_dps)}</b> dps &middot; ${dur(enc.duration)}`;
      renderRows(enc);
      renderMetric(el.healRows, enc, "healing", "hps", "heal_share", "No healing yet");
      renderMetric(el.takenRows, enc, "damage_taken", null, "taken_share", "No damage taken");
      renderSub(snap, enc);
    }

    renderXp(snap);
    renderAuras(snap.auras || []);
    renderDebuffs(snap.debuffs || []);
    renderBuffs(snap.buffs || []);
    el.status.classList.toggle("hidden", snap.live !== false);
  }

  function renderRows(enc) {
    let meter = enc.meter || [];
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
        const tag = r.is_pet ? '<span class="tag">charmed</span>' : "";
        return `<div class="${cls.join(" ")}" style="--share:${(r.share * 100).toFixed(1)}%">
          <span class="name">${esc(r.name)}${tag}</span>
          <span class="nums">
            <span class="dmg">${num(r.damage)}</span>
            <span class="dps">${num(r.dps)}</span>
          </span>
        </div>`;
      })
      .join("");
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
     level. The bar is the headline; the numbers are secondary. */
  function renderXp(snap) {
    const x = snap.exp;
    if (!x) return;
    const lvl = snap.level || 0;
    el.xpTitle.textContent = `Level ${lvl}`;
    const bits = [];
    if (x.rate_per_hour > 0) bits.push(`<b>${x.rate_per_hour.toFixed(1)}</b> %/hr`);
    if (x.aa_per_hour > 0) bits.push(`<b>${x.aa_per_hour.toFixed(1)}</b> AA/hr`);
    el.xpMeta.innerHTML = bits.join(" &middot; ");
    const pct = Math.max(0, Math.min(100, x.percent));
    const eta = x.seconds_to_next_level != null ? dur(x.seconds_to_next_level) : "—";
    el.xpRows.innerHTML = `<div class="row you" style="--share:${pct.toFixed(1)}%">
      <span class="name">${pct.toFixed(1)}% &rarr; ${lvl + 1}</span>
      <span class="nums">
        <span class="dmg">${x.kills} kills</span>
        <span class="dps">${eta}</span>
      </span></div>
      <div class="row" style="--share:0%">
        <span class="name">AA</span>
        <span class="nums">
          <span class="dmg">${x.aa_unspent} unspent</span>
          <span class="dps">${x.aa_earned}</span>
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
        const pct = b.permanent
          ? 100
          : Math.max(0, Math.min(100, (b.remaining / total) * 100));
        const cls = ["buff", `cat-${(b.category || "other").replace(/[^a-z]/g, "")}`];
        if (!b.permanent) {
          if (b.remaining <= 10) cls.push("urgent");
          else if (b.remaining <= 30) cls.push("expiring");
        }
        // Landing text is ambiguous for many spells; flag low-confidence
        // identifications rather than presenting a guess as fact.
        if (b.confidence < 0.8) cls.push("unsure");
        return `<div class="${cls.join(" ")}" style="--remain:${pct.toFixed(1)}%">
          <span class="bname">${esc(b.name)}</span>
          <span class="btime">${b.permanent ? "&infin;" : dur(b.remaining)}</span>
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
        : { ...b, remaining: Math.max(0, b.remaining - 1) }
    );
    renderBuffs(lastSnapshot.buffs);
  }, 1000);
})();
