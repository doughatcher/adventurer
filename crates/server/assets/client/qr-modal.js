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
      background: var(--bg2, #161b22);
      color: var(--text, #d6deea);
      border: 1px solid var(--line, #2a313c);
      border-radius: 6px;
      padding: 4px 10px; margin-right: 6px;
      cursor: pointer; font-size: 13px;
    }
    #adv-players-btn:hover { background: var(--card, #1c222b); }
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

  // ─── inject the header button ───
  function injectButton() {
    const actions = document.querySelector('.header-actions');
    if (!actions || $('adv-players-btn')) return;
    const btn = document.createElement('button');
    btn.id = 'adv-players-btn';
    btn.title = 'Show join QR + players';
    btn.innerHTML = '&#9863; Players';
    btn.addEventListener('click', open);
    actions.insertBefore(btn, actions.firstChild);
  }
  // The dnd-stage UI builds the header before this script runs (defer), but
  // wait for DOM ready just in case.
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', injectButton);
  } else {
    injectButton();
  }

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
