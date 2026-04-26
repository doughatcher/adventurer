// Adventure Log — Player view client.
//
// On load:
//   1. Pick (or create) a stable per-device token in localStorage.
//   2. POST /api/players/announce so the DM sees us in the QR modal.
//   3. Open WS at /ws — listen for state, panels, transcript, player_assigned.
//   4. Render character (when assigned), scene, party, transcript, decisions.
//
// We connect to the SAME /ws as the DM stage. Players see the same broadcast
// stream — we just render a stripped, mobile-friendly subset.

(() => {
  const $ = id => document.getElementById(id);
  const enc = encodeURIComponent;

  // ─── token / identity ───
  const TOKEN_KEY = 'adv-player-token';
  function getToken() {
    let t = localStorage.getItem(TOKEN_KEY);
    if (!t) {
      t = 'p_' + Math.random().toString(36).slice(2, 10) + Math.random().toString(36).slice(2, 10);
      localStorage.setItem(TOKEN_KEY, t);
    }
    return t;
  }
  const token = getToken();
  const label = (() => {
    // Friendly device name from UA — DM sees this in the assignment dropdown.
    const ua = navigator.userAgent;
    if (/iPhone|iPad/.test(ua))         return 'iPhone';
    if (/Android/.test(ua))             return 'Android';
    if (/Macintosh/.test(ua))           return 'Mac';
    if (/Windows/.test(ua))             return 'Windows';
    return 'Browser';
  })();

  // ─── state we track locally ───
  let myCharacterSlug = null;   // assigned by the DM
  let lastFullState = {};       // mirror of the latest /api/state

  // ─── announce ───
  fetch('/api/players/announce', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ token, label }),
  }).catch(e => console.warn('announce failed', e));

  // ─── status indicator ───
  function setStatus(s) {
    const el = $('hdr-status');
    el.textContent = s === 'connected' ? 'Live' : (s === 'connecting' ? 'Connecting…' : 'Disconnected');
    el.className = s;
  }

  // ─── render helpers ───
  function renderMe() {
    const meCard  = $('me-card');
    const meEmpty = $('me-empty');
    if (!myCharacterSlug) {
      meCard.classList.add('hidden');
      meEmpty.classList.remove('hidden');
      return;
    }
    const chars = (lastFullState.characters || {});
    const me = chars[myCharacterSlug];
    if (!me) {
      meCard.classList.add('hidden');
      meEmpty.classList.remove('hidden');
      $('me-empty').textContent = `Character "${myCharacterSlug}" not in state yet — waiting for the DM.`;
      return;
    }
    meEmpty.classList.add('hidden');
    meCard.classList.remove('hidden');
    $('me-name').textContent = me.name || myCharacterSlug;
    const meta = [];
    if (me.class) meta.push(me.class);
    if (me.ac != null && me.ac !== 0) meta.push(`AC ${me.ac}`);
    $('me-meta').textContent = meta.join(' · ');

    const hp = me.hp ?? 0, max = me.max_hp ?? 0;
    const pct = max > 0 ? Math.max(0, Math.min(1, hp / max)) : 0;
    const bar = $('me-hp-bar');
    bar.classList.toggle('bloodied', pct > 0 && pct < 0.5);
    bar.classList.toggle('dying',    pct > 0 && pct < 0.2);
    $('me-hp-fill').style.width = `${pct * 100}%`;
    $('me-hp-label').textContent = max > 0 ? `HP ${hp} / ${max}` : `HP ${hp}`;

    const conds = $('me-conds');
    conds.innerHTML = '';
    (me.conditions || []).forEach(c => {
      const span = document.createElement('span');
      span.className = 'cond';
      span.textContent = c;
      conds.appendChild(span);
    });

    $('me-notes').textContent = me.notes || '';
  }

  function renderParty() {
    const list = $('party-list');
    const chars = lastFullState.characters || {};
    list.innerHTML = '';
    const slugs = Object.keys(chars);
    if (slugs.length === 0) {
      list.innerHTML = '<div class="empty">No party data yet.</div>';
      return;
    }
    // PCs first, then enemies; alphabetic within group.
    const pcs = slugs.filter(s => !chars[s].is_enemy).sort();
    const ens = slugs.filter(s =>  chars[s].is_enemy).sort();
    for (const slug of [...pcs, ...ens]) {
      const c = chars[slug];
      const row = document.createElement('div');
      row.className = 'party-row';
      if (c.is_enemy) row.classList.add('enemy');
      if (slug === myCharacterSlug) row.classList.add('is-me');
      const nameWrap = document.createElement('div');
      const name = document.createElement('div');
      name.className = 'pname';
      name.textContent = c.name || slug;
      nameWrap.appendChild(name);
      if (c.class) {
        const cls = document.createElement('div');
        cls.className = 'pclass';
        cls.textContent = c.class;
        nameWrap.appendChild(cls);
      }
      const hp = document.createElement('div');
      hp.className = 'php';
      const h = c.hp == null ? '?' : c.hp;
      const m = c.max_hp == null ? '?' : c.max_hp;
      hp.textContent = `HP ${h}/${m}` + (c.status === 'dead' ? ' ☠' : '');
      row.appendChild(nameWrap);
      row.appendChild(hp);
      list.appendChild(row);
    }
  }

  function renderPanels(panels) {
    if (panels.scene) {
      const body = panels.scene.replace(/^## PANEL: scene\s*/i, '').trim();
      $('scene-body').textContent = body;
    }
    if (panels['next-steps']) {
      const body = panels['next-steps'].replace(/^## PANEL: next-steps\s*/i, '').trim();
      $('next-body').textContent = body;
    }
  }

  function renderTranscript(tail) {
    $('transcript-tail').textContent = tail || '';
  }

  function showDecision(d) {
    $('decision-title').textContent = d.title || 'Choose…';
    $('decision-context').textContent = d.context || '';
    const opts = $('decision-options');
    opts.innerHTML = '';
    (d.options || []).forEach(o => {
      const div = document.createElement('div');
      div.className = 'dec-option';
      const n = document.createElement('div');
      n.className = 'dec-name';
      n.textContent = o.name || '';
      const desc = document.createElement('div');
      desc.className = 'dec-desc';
      desc.textContent = o.desc || '';
      const det = document.createElement('div');
      det.className = 'dec-detail';
      det.textContent = o.detail || '';
      div.appendChild(n);
      if (o.desc)   div.appendChild(desc);
      if (o.detail) div.appendChild(det);
      opts.appendChild(div);
    });
    $('decision-overlay').classList.remove('hidden');
  }
  $('decision-close').addEventListener('click', () => {
    $('decision-overlay').classList.add('hidden');
  });

  // ─── WS ───
  let ws = null;
  let backoff = 1000;
  function connect() {
    setStatus('connecting');
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}/ws?role=player&token=${enc(token)}`;
    ws = new WebSocket(url);
    ws.addEventListener('open', () => {
      setStatus('connected');
      backoff = 1000;
    });
    ws.addEventListener('close', () => {
      setStatus('disconnected');
      setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, 15000);
    });
    ws.addEventListener('error', () => {/* close handler will reconnect */});
    ws.addEventListener('message', (ev) => {
      let msg;
      try { msg = JSON.parse(ev.data); } catch { return; }
      handleEvent(msg);
    });
  }

  function handleEvent(msg) {
    switch (msg.type) {
      case 'init':
        lastFullState = msg.state || {};
        renderPanels(msg.panels || {});
        renderTranscript(tailOf(msg.transcript || ''));
        renderMe(); renderParty();
        break;
      case 'state':
        lastFullState = msg.data || {};
        renderMe(); renderParty();
        break;
      case 'panels':
        renderPanels(msg.data || {});
        break;
      case 'transcript':
        renderTranscript(msg.tail || '');
        break;
      case 'decision':
        showDecision(msg.data || {});
        break;
      case 'player_assigned':
        if (msg.player && msg.player.token === token) {
          myCharacterSlug = msg.player.character;
          renderMe(); renderParty();
        }
        break;
    }
  }
  function tailOf(transcript) {
    const lines = transcript.split('\n');
    return lines.slice(-12).join('\n');
  }

  // Get our current assignment (in case the DM assigned before WS opened).
  fetch('/api/players').then(r => r.json()).then(list => {
    const me = list.find(p => p.token === token);
    if (me && me.character) {
      myCharacterSlug = me.character;
      renderMe();
    }
  }).catch(() => {});

  connect();
})();
