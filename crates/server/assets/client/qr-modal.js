// Adventure Log — QR / players overlay for the DM stage.
//
// Loaded on top of the vanilla-JS dnd-stage UI without modifying it. We:
//   - inject a "&#9863; Players" button into the header
//   - inject a modal showing the QR + connected-player list
//   - listen on the existing /ws for player_joined / player_assigned events
//   - allow the DM to assign a character (slug) to each player
//
// Player data lives server-side; we just render + post.

(() => {
  if (window.__advQrModalInit) return;
  window.__advQrModalInit = true;

  const $ = id => document.getElementById(id);

  // ─── inject the modal DOM ───
  const STYLES = `
    #adv-players-btn {
      background: rgba(244,185,66,.18);
      color: #f4b942;
      border: 2px solid #f4b942;
      border-radius: 8px;
      padding: 6px 14px; margin-right: 8px;
      cursor: pointer; font-size: 14px; font-weight: 600;
    }
    #adv-players-btn:hover { background: rgba(244,185,66,.3); }
    #adv-start-players-btn,
    #adv-test-mode-btn,
    #adv-continue-btn {
      display: block; margin: 12px auto 0;
      border-radius: 10px;
      padding: 14px 28px; font-size: 17px; font-weight: 600;
      cursor: pointer;
      width: 90%; max-width: 480px;
    }
    #adv-start-players-btn {
      background: rgba(244,185,66,.18); color: #f4b942; border: 2px solid #f4b942;
    }
    #adv-start-players-btn:hover { background: rgba(244,185,66,.3); }
    #adv-test-mode-btn {
      background: rgba(105,209,149,.14); color: #69d195; border: 2px dashed #69d195;
    }
    #adv-test-mode-btn:hover { background: rgba(105,209,149,.25); }
    #adv-continue-btn {
      background: rgba(99,168,232,.14); color: #63a8e8; border: 2px solid #63a8e8;
    }
    #adv-continue-btn:hover { background: rgba(99,168,232,.28); }
    #adv-continue-overlay {
      position: fixed; inset: 0;
      background: rgba(0,0,0,.7);
      display: none; align-items: center; justify-content: center;
      z-index: 9050;
    }
    #adv-continue-overlay.show { display: flex; }
    #adv-continue-modal {
      width: min(640px, 92vw); max-height: 80vh;
      background: #1c222b; color: #d6deea;
      border: 2px solid #63a8e8; border-radius: 14px;
      padding: 22px;
      overflow: auto;
    }
    #adv-continue-modal h2 { margin: 0 0 10px; color: #63a8e8; }
    #adv-continue-list {
      display: flex; flex-direction: column; gap: 6px; margin: 12px 0;
    }
    .adv-cont-row {
      display: flex; justify-content: space-between; align-items: center;
      padding: 10px 14px;
      background: #161b22; border: 1px solid #2a313c; border-radius: 8px;
      cursor: pointer; font-family: ui-monospace, monospace; font-size: 14px;
    }
    .adv-cont-row:hover { border-color: #63a8e8; }
    .adv-cont-row .adv-cont-id { color: #d6deea; }
    .adv-cont-row .adv-cont-tag { font-size: 11px; color: #5b6470; }
    #adv-mode-banner {
      position: fixed; bottom: 8px; left: 50%;
      transform: translateX(-50%);
      background: rgba(105,209,149,.95); color: #0e1116;
      font-weight: 600;
      padding: 5px 14px; z-index: 9700;
      font-size: 12px; letter-spacing: .04em;
      border-radius: 999px;
      pointer-events: none;
      box-shadow: 0 2px 12px rgba(0,0,0,.5);
    }
    #adv-qr-overlay {
      position: fixed; inset: 0;
      background: rgba(0,0,0,.7);
      display: none;
      align-items: center; justify-content: center;
      z-index: 9000;
    }
    #adv-qr-overlay.show { display: flex; }
    #adv-qr-modal {
      width: min(720px, 92vw); max-height: 90vh;
      background: #1c222b; color: #d6deea;
      border: 2px solid #f4b942;
      border-radius: 14px;
      padding: 24px;
      display: grid; grid-template-columns: minmax(260px, 320px) 1fr; gap: 24px;
      overflow: auto;
    }
    @media (max-width: 720px) {
      #adv-qr-modal { grid-template-columns: 1fr; }
    }
    #adv-qr-modal h2 { margin: 0 0 6px; color: #f4b942; }
    #adv-qr-modal .muted { color: #8d96a7; }
    #adv-qr-svg { background: #fff; padding: 12px; border-radius: 8px; }
    #adv-qr-svg svg { width: 100%; height: auto; display: block; }
    #adv-qr-url {
      margin-top: 10px; font-family: ui-monospace, monospace;
      background: #0e1116; padding: 8px 10px; border-radius: 6px;
      word-break: break-all; font-size: 13px;
    }
    #adv-players-list { display: flex; flex-direction: column; gap: 10px; }
    .adv-player {
      padding: 10px 12px;
      border: 1px solid #2a313c;
      border-radius: 8px;
      background: #161b22;
    }
    .adv-player .row {
      display: flex; align-items: center; justify-content: space-between; gap: 8px;
    }
    .adv-player .label { font-weight: 600; }
    .adv-player .token { color: #5b6470; font-family: ui-monospace, monospace; font-size: 11px; }
    .adv-player select {
      margin-top: 8px; width: 100%;
      background: #0e1116; color: #d6deea; border: 1px solid #2a313c;
      padding: 6px 8px; border-radius: 6px; font-size: 13px;
    }
    .adv-player.assigned { border-color: #f4b942; }
    .adv-player .assigned-tag {
      display: inline-block; background: rgba(244,185,66,.18); color: #f4b942;
      padding: 2px 8px; border-radius: 999px; font-size: 11px;
    }
    #adv-qr-close {
      position: absolute; top: 14px; right: 18px;
      background: transparent; border: none; color: #8d96a7;
      font-size: 22px; cursor: pointer;
    }
    .adv-empty-players {
      color: #5b6470; font-style: italic; padding: 20px 0;
      text-align: center;
    }
    .adv-cfg-row { display: flex; align-items: center; gap: 10px; margin: 6px 0; }
    .adv-cfg-row label {
      width: 60px; color: #8d96a7; font-size: 12px;
      text-transform: uppercase; letter-spacing: .06em;
    }
    .adv-cfg-row input {
      flex: 1; background: #0e1116; color: #d6deea;
      border: 1px solid #2a313c; padding: 6px 10px; border-radius: 6px;
      font-size: 13px;
    }
    .adv-btn {
      background: #2a313c; color: #d6deea;
      border: 1px solid #3b4452; border-radius: 6px;
      padding: 7px 14px; cursor: pointer; font-size: 13px;
    }
    .adv-btn.primary { background: rgba(244,185,66,.18); color: #f4b942; border-color: #f4b942; }
    .adv-btn:hover { filter: brightness(1.15); }
    #adv-push-result.ok { color: #69d195; }
    #adv-push-result.bad { color: #e85a5a; }
  `;
  const styleEl = document.createElement('style');
  styleEl.textContent = STYLES;
  document.head.appendChild(styleEl);

  const overlay = document.createElement('div');
  overlay.id = 'adv-qr-overlay';
  overlay.innerHTML = `
    <div id="adv-qr-modal" style="position: relative;">
      <button id="adv-qr-close" title="Close">&#10005;</button>
      <div>
        <h2>Scan to join</h2>
        <div class="muted" style="font-size:13px;margin-bottom:12px">
          Players: scan with phone camera. New devices appear on the right.
        </div>
        <div id="adv-qr-svg">Loading…</div>
        <div id="adv-qr-url"></div>

        <h2 style="margin-top:24px">Backup to GitHub</h2>
        <div class="muted" style="font-size:13px;margin-bottom:10px">
          Pushes <code>data/sessions/&lt;id&gt;/{transcript,state,panels}</code> as
          a single atomic commit. The repo's existing GH Action does the
          journal generation + Hugo deploy.
        </div>
        <div class="adv-cfg-row">
          <label for="adv-cfg-repo">Repo</label>
          <input id="adv-cfg-repo" placeholder="owner/repo" autocomplete="off">
        </div>
        <div class="adv-cfg-row">
          <label for="adv-cfg-branch">Branch</label>
          <input id="adv-cfg-branch" placeholder="main" autocomplete="off">
        </div>
        <div class="adv-cfg-row">
          <label for="adv-cfg-pat">PAT</label>
          <input id="adv-cfg-pat" type="password" placeholder="(set — leave blank to keep)" autocomplete="off">
        </div>
        <div style="display:flex;gap:8px;margin-top:8px">
          <button class="adv-btn" id="adv-cfg-save">Save settings</button>
          <button class="adv-btn primary" id="adv-do-push">⤴ Save session now</button>
        </div>
        <div id="adv-push-result" style="margin-top:10px;font-size:13px"></div>
      </div>
      <div>
        <h2>Players</h2>
        <div class="muted" style="font-size:13px;margin-bottom:10px">
          Pick a character for each connected device.
        </div>
        <div id="adv-players-list"><div class="adv-empty-players">No one yet.</div></div>
      </div>
    </div>
  `;
  document.body.appendChild(overlay);
  overlay.addEventListener('click', e => { if (e.target === overlay) close(); });
  $('adv-qr-close').addEventListener('click', close);

  // ─── inject the header button + start-overlay button ───
  // The dnd-stage UI shows a "start-overlay" with "Start New Session" until
  // the DM clicks Start; the header (and our header button) is hidden
  // behind it. Inject a second prominent QR button INTO the start-overlay
  // so the DM can show the QR to players BEFORE starting the session.
  function injectButtons() {
    // Header button (live-session view)
    const actions = document.querySelector('.header-actions');
    if (actions && !$('adv-players-btn')) {
      const btn = document.createElement('button');
      btn.id = 'adv-players-btn';
      btn.title = 'Show join QR + players';
      btn.innerHTML = '&#9863; Players';
      btn.addEventListener('click', open);
      actions.insertBefore(btn, actions.firstChild);
    }
    // Start-overlay button (initial view, before session starts)
    const startBox = document.getElementById('start-box');
    if (startBox && !$('adv-start-players-btn')) {
      const startBtn = document.createElement('button');
      startBtn.id = 'adv-start-players-btn';
      startBtn.innerHTML = '&#9863; Show QR for players to join';
      startBtn.addEventListener('click', open);
      const loadBtn = document.getElementById('btn-start-load-history');
      if (loadBtn && loadBtn.parentNode) {
        loadBtn.parentNode.insertBefore(startBtn, loadBtn.nextSibling);
      } else {
        startBox.appendChild(startBtn);
      }
    }
    // Start-overlay: ⏬ Continue from adventure-log — pulls the latest
    // archived session (transcript + state + panels) from the configured
    // GitHub repo and seeds the running session with it. Tomorrow's gameplay
    // starts from where last week's left off.
    if (startBox && !$('adv-continue-btn')) {
      const contBtn = document.createElement('button');
      contBtn.id = 'adv-continue-btn';
      contBtn.innerHTML = '&#8675; Continue from GitHub session…';
      contBtn.title = 'List archived sessions from the configured adventure-log repo';
      contBtn.addEventListener('click', openContinueModal);
      const target = $('adv-start-players-btn') ||
                     document.getElementById('btn-start-load-history');
      if (target && target.parentNode) {
        target.parentNode.insertBefore(contBtn, target.nextSibling);
      } else {
        startBox.appendChild(contBtn);
      }
    }
    // Start-overlay: 🧪 Test Mode button — flags session as ephemeral so
    // /api/session/save refuses (no GitHub commit), then dismisses overlay
    // by triggering the same code path the regular "Start New Session"
    // button uses.
    if (startBox && !$('adv-test-mode-btn')) {
      const testBtn = document.createElement('button');
      testBtn.id = 'adv-test-mode-btn';
      testBtn.innerHTML = '&#129514; Start in Test Mode (no GitHub save)';
      testBtn.title = 'Use for mic / pipeline tests — nothing gets pushed to the content repo';
      testBtn.addEventListener('click', async () => {
        try {
          await fetch('/api/session/mode', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ mode: 'test' }),
          });
        } catch (e) { console.warn('set mode failed', e); }
        // Open the QR modal immediately — main reason for Test Mode is
        // iPad / phone client testing, so the QR is what the DM wants to
        // hand off next.
        open();
        // ALSO trigger the regular session-start so the full UI is live
        // behind the QR modal — that way after the user closes the modal
        // they're already in-session and can hit "Tap to talk" on the iPad.
        const realStart = document.getElementById('btn-start-session');
        if (realStart) realStart.click();
        refreshSessionMode();   // pop the banner immediately
      });
      const target = $('adv-start-players-btn') ||
                     document.getElementById('btn-start-load-history');
      if (target && target.parentNode) {
        target.parentNode.insertBefore(testBtn, target.nextSibling);
      } else {
        startBox.appendChild(testBtn);
      }
    }
    // Live banner when in test mode (refresh from server periodically).
    refreshSessionMode();
  }

  // ─── Continue-from-adventure-log modal ───
  // Inject a one-shot modal we keep around (faster reopens, less DOM churn).
  function ensureContinueModal() {
    if (document.getElementById('adv-continue-overlay')) return;
    const ov = document.createElement('div');
    ov.id = 'adv-continue-overlay';
    ov.innerHTML = `
      <div id="adv-continue-modal">
        <h2>&#8675; Continue from a saved session</h2>
        <div class="muted" style="font-size:13px;color:#8d96a7">
          Loads <code>data/sessions/&lt;id&gt;/{transcript,state,panels}</code>
          from your configured adventure-log repo and seeds the running session
          with it. Subsequent saves will UPDATE that session folder.
        </div>
        <div id="adv-continue-list">Loading…</div>
        <div style="display:flex;gap:8px;margin-top:12px">
          <button class="adv-btn" id="adv-continue-cancel">Cancel</button>
        </div>
        <div id="adv-continue-result" style="margin-top:10px;font-size:13px"></div>
      </div>
    `;
    document.body.appendChild(ov);
    ov.addEventListener('click', e => {
      if (e.target === ov) closeContinueModal();
    });
    document.getElementById('adv-continue-cancel').addEventListener('click', closeContinueModal);
  }

  function closeContinueModal() {
    const ov = document.getElementById('adv-continue-overlay');
    if (ov) ov.classList.remove('show');
  }

  async function openContinueModal() {
    ensureContinueModal();
    const ov = document.getElementById('adv-continue-overlay');
    ov.classList.add('show');
    const list = document.getElementById('adv-continue-list');
    const res  = document.getElementById('adv-continue-result');
    list.innerHTML = 'Loading…';
    res.textContent = '';
    try {
      const r = await fetch('/api/adventure-log/sessions');
      const body = await r.json();
      if (!r.ok || !body.ok) {
        list.innerHTML = `<div style="color:#e85a5a">Couldn't list sessions: ${body.error || r.status}</div>`;
        return;
      }
      const sessions = body.sessions || [];
      if (!sessions.length) {
        list.innerHTML = '<div style="color:#5b6470">No archived sessions found.</div>';
        return;
      }
      list.innerHTML = '';
      // Limit to 30 most-recent so the list stays scannable.
      sessions.slice(0, 30).forEach((id, i) => {
        const row = document.createElement('div');
        row.className = 'adv-cont-row';
        row.innerHTML = `
          <span class="adv-cont-id">${id}</span>
          <span class="adv-cont-tag">${i === 0 ? 'most recent' : ''}</span>
        `;
        row.addEventListener('click', () => loadSession(id));
        list.appendChild(row);
      });
    } catch (e) {
      list.innerHTML = `<div style="color:#e85a5a">Error: ${e}</div>`;
    }
  }

  async function loadSession(id) {
    const res = document.getElementById('adv-continue-result');
    res.textContent = `Loading ${id}…`;
    res.style.color = '#8d96a7';
    try {
      const r = await fetch('/api/session/load', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ session_id: id }),
      });
      const body = await r.json();
      if (!r.ok || !body.ok) {
        res.style.color = '#e85a5a';
        res.textContent = `Failed: ${body.error || r.status}`;
        return;
      }
      res.style.color = '#69d195';
      res.textContent = `Loaded ${body.session_id} (${body.transcript_len} chars). Starting session…`;
      // Trigger the regular session-start so the live UI takes over.
      const realStart = document.getElementById('btn-start-session');
      if (realStart) realStart.click();
      setTimeout(closeContinueModal, 400);
    } catch (e) {
      res.style.color = '#e85a5a';
      res.textContent = `Error: ${e}`;
    }
  }

  let modeBannerEl = null;
  async function refreshSessionMode() {
    try {
      const r = await fetch('/api/session');
      if (!r.ok) return;
      const { mode } = await r.json();
      setModeBanner(mode === 'test');
    } catch {}
  }
  function setModeBanner(isTest) {
    if (!isTest) {
      if (modeBannerEl) { modeBannerEl.remove(); modeBannerEl = null; }
      return;
    }
    if (modeBannerEl) return;
    modeBannerEl = document.createElement('div');
    modeBannerEl.id = 'adv-mode-banner';
    modeBannerEl.innerHTML = '&#129514; TEST MODE — nothing is being saved to GitHub';
    document.body.appendChild(modeBannerEl);
  }
  // Refresh mode every 10s so the banner stays current if mode changes
  // server-side (e.g., curl POST /api/session/mode).
  setInterval(refreshSessionMode, 10000);

  // Inject ASAP and re-check periodically — stage.js may build/show overlays
  // dynamically, and the dnd-stage UI sometimes rebuilds the header.
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', injectButtons);
  } else {
    injectButtons();
  }
  setInterval(injectButtons, 2000);

  // ─── data ───
  let players = [];
  let characters = [];   // [{slug, name}, …]
  let lanInfo = null;

  function open() {
    overlay.classList.add('show');
    refreshAll();
  }
  function close() { overlay.classList.remove('show'); }

  async function refreshAll() {
    try {
      // LAN info & QR — only need to fetch once per modal open.
      if (!lanInfo) {
        const r = await fetch('/api/lan-info');
        lanInfo = await r.json();
        $('adv-qr-svg').innerHTML = lanInfo.qr_svg || '(no QR)';
        $('adv-qr-url').textContent = lanInfo.join_url || '';
      }
      // Players — refresh always.
      const pr = await fetch('/api/players');
      players = await pr.json();
      // Pull current characters from /api/state for the dropdown options.
      const sr = await fetch('/api/state');
      const state = await sr.json();
      characters = Object.entries(state.characters || {})
        .filter(([_, c]) => !c.is_enemy)
        .map(([slug, c]) => ({ slug, name: c.name || slug }));
      renderPlayers();
      // GitHub sync settings — load each open so we reflect any env-var-set state.
      const cr = await fetch('/api/config');
      const cfg = await cr.json();
      $('adv-cfg-repo').value = cfg.repo || '';
      $('adv-cfg-branch').value = cfg.branch || 'main';
      $('adv-cfg-pat').placeholder = cfg.has_pat
        ? '(set — leave blank to keep)'
        : '(unset — paste a PAT)';
    } catch (e) {
      console.warn('refreshAll failed', e);
    }
  }

  // Settings save
  document.addEventListener('click', async (e) => {
    if (e.target && e.target.id === 'adv-cfg-save') {
      const body = {
        repo:   $('adv-cfg-repo').value.trim(),
        branch: $('adv-cfg-branch').value.trim() || 'main',
      };
      const pat = $('adv-cfg-pat').value;
      if (pat) body.pat = pat;
      const r = await fetch('/api/config', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify(body),
      });
      const result = $('adv-push-result');
      if (r.ok) {
        $('adv-cfg-pat').value = '';
        result.className = 'ok';
        result.textContent = 'Saved.';
        // Refresh has_pat indicator
        refreshAll();
      } else {
        result.className = 'bad';
        result.textContent = 'Failed: ' + await r.text();
      }
    }
    if (e.target && e.target.id === 'adv-do-push') {
      const result = $('adv-push-result');
      result.className = '';
      result.textContent = 'Pushing…';
      const r = await fetch('/api/session/save', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({}),
      });
      const body = await r.json();
      if (r.ok && body.ok) {
        result.className = 'ok';
        result.innerHTML =
          `Saved → <a href="${body.commit_url}" target="_blank" rel="noreferrer" style="color:inherit">` +
          `${body.commit_sha.slice(0,7)}</a> (${body.files} files)`;
      } else {
        result.className = 'bad';
        result.textContent = 'Failed: ' + (body.error || 'unknown');
      }
    }
  });

  function renderPlayers() {
    const list = $('adv-players-list');
    if (!players.length) {
      list.innerHTML = '<div class="adv-empty-players">No one has scanned yet.</div>';
      return;
    }
    list.innerHTML = '';
    for (const p of players) {
      const wrap = document.createElement('div');
      wrap.className = 'adv-player' + (p.character ? ' assigned' : '');
      const head = document.createElement('div');
      head.className = 'row';
      const labelHtml = p.label ? `<span class="label">${p.label}</span>` : '<span class="label">Player</span>';
      const tagHtml = p.character
        ? `<span class="assigned-tag">${escape(charName(p.character))}</span>`
        : '';
      head.innerHTML = `${labelHtml} ${tagHtml}`;
      wrap.appendChild(head);
      const tok = document.createElement('div');
      tok.className = 'token';
      tok.textContent = p.token.slice(0, 14) + '…';
      wrap.appendChild(tok);
      const sel = document.createElement('select');
      sel.innerHTML =
        '<option value="">(unassigned)</option>' +
        characters.map(c => `<option value="${escape(c.slug)}"${c.slug === p.character ? ' selected' : ''}>${escape(c.name)}</option>`).join('');
      sel.addEventListener('change', async () => {
        const character = sel.value || null;
        const r = await fetch(`/api/players/${encodeURIComponent(p.token)}/assign`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ character }),
        });
        if (!r.ok) console.warn('assign failed', await r.text());
      });
      wrap.appendChild(sel);
      list.appendChild(wrap);
    }
  }
  function charName(slug) {
    const c = characters.find(x => x.slug === slug);
    return c ? c.name : slug;
  }
  function escape(s) {
    return String(s).replace(/[&<>"']/g, ch => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    }[ch]));
  }

  // ─── listen on the DM's WebSocket too ───
  // Easiest path: open a SECOND ws subscription dedicated to player events.
  // (We can't easily hook the existing stage.js ws.) Cheap & fine — broadcasts
  // are tiny.
  function connectMonitor() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(`${proto}//${location.host}/ws?role=dm-monitor`);
    ws.addEventListener('message', (ev) => {
      let msg; try { msg = JSON.parse(ev.data); } catch { return; }
      if (msg.type === 'player_joined' || msg.type === 'player_assigned') {
        refreshAll();
      }
    });
    ws.addEventListener('close', () => setTimeout(connectMonitor, 2000));
  }
  connectMonitor();
})();
