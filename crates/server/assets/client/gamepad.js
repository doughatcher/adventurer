// Adventure Log — controller / Steam Input overlay for the DM stage.
//
// Loaded alongside qr-modal.js (additive, no changes to the vendored
// stage.js). Gives the page basic controller-driven navigation so the UI
// is usable from the couch.
//
// TWO input paths supported:
//
//  1. **Steam Input → keyboard** (the common case). Steam intercepts the
//     gamepad and feeds keyboard events to the focused window using its
//     "Desktop Configuration" (default mapping for non-Steam games):
//        D-pad / left stick  →  arrow keys
//        A button            →  Enter
//        B button            →  Escape
//        Left trackpad / RS  →  mouse cursor / scroll
//     We bind arrow keys for focus navigation, Enter to click. Escape is
//     already handled by the existing modal close handlers. Right-stick mouse
//     control + A=click work without any code from us.
//
//  2. **Raw Gamepad API** (works only when Steam Input is OFF for this game,
//     or via Steam's "Allow Gamepad API to read raw inputs" setting). We poll
//     navigator.getGamepads() and translate buttons/axes the same way.
//
// Focus ring: thick yellow outline on the focused element while controller
// nav is active. Clicking with a mouse hides it; pressing arrow keys or any
// controller button brings it back.

(() => {
  if (window.__advGamepadInit) return;
  window.__advGamepadInit = true;

  // ─── focus ring CSS ───
  const css = document.createElement('style');
  css.textContent = `
    body.adv-gp-active .adv-gp-focus,
    body.adv-gp-active .adv-gp-focus:focus {
      outline: 3px solid #f4b942 !important;
      outline-offset: 2px !important;
      box-shadow: 0 0 0 4px rgba(244,185,66,.25) !important;
      border-radius: 4px;
    }
    #adv-gp-status {
      position: fixed; bottom: 8px; left: 8px;
      z-index: 9500;
      font-size: 11px; font-family: ui-monospace, monospace;
      color: #5b6470;
      background: rgba(14,17,22,.6);
      padding: 4px 10px; border-radius: 999px;
      pointer-events: none;
      transition: opacity .3s;
      opacity: 0;
    }
    #adv-gp-status.show { opacity: 1; color: #69d195; }
  `;
  document.head.appendChild(css);

  const status = document.createElement('div');
  status.id = 'adv-gp-status';
  status.textContent = '🎮 controller';
  document.body.appendChild(status);

  // ─── input debug overlay ───
  // Bottom-right diagnostic showing the last key/mouse/pad event. Hidden by
  // default — toggle with the discreet 🔍 chip OR the backtick (`) key OR
  // visit the page with ?debug=1. Server-side input logging keeps running
  // either way, so the panel is purely for live local visibility.
  const debugStyle = document.createElement('style');
  debugStyle.textContent = `
    #adv-input-debug {
      position: fixed; bottom: 8px; right: 8px;
      z-index: 9600;
      font-family: ui-monospace, monospace; font-size: 11px;
      color: #d6deea; background: rgba(14,17,22,.92);
      border: 1px solid #2a313c; border-radius: 6px;
      padding: 6px 10px; min-width: 240px;
      pointer-events: none;
      display: none;
    }
    #adv-input-debug.show { display: block; }
    #adv-input-debug .label { color: #5b6470; }
    #adv-input-debug .value { color: #f4b942; }
    #adv-input-debug .stale { color: #5b6470; }
    #adv-input-debug .row { display: flex; justify-content: space-between; gap: 8px; }
    #adv-debug-toggle {
      position: fixed; bottom: 6px; right: 6px;
      z-index: 9601;
      width: 28px; height: 28px;
      border-radius: 999px;
      background: rgba(14,17,22,.5);
      border: 1px solid rgba(42,49,60,.6);
      color: #5b6470;
      font-size: 13px; line-height: 1;
      display: flex; align-items: center; justify-content: center;
      cursor: pointer;
      opacity: .35;
      transition: opacity .15s, color .15s;
      user-select: none;
    }
    #adv-debug-toggle:hover { opacity: 1; color: #d6deea; }
    #adv-debug-toggle.active { color: #f4b942; opacity: .9; }
    #adv-input-debug.show ~ #adv-debug-toggle,
    #adv-debug-toggle.shifted { bottom: auto; top: 6px; right: 6px; }
  `;
  document.head.appendChild(debugStyle);
  const dbg = document.createElement('div');
  dbg.id = 'adv-input-debug';
  dbg.innerHTML = `
    <div class="row"><span class="label">key:</span>     <span id="adv-dbg-key" class="value">—</span></div>
    <div class="row"><span class="label">mouse:</span>   <span id="adv-dbg-mouse" class="value">—</span></div>
    <div class="row"><span class="label">pads:</span>    <span id="adv-dbg-pads" class="value">0</span></div>
    <div class="row"><span class="label">last btn:</span><span id="adv-dbg-pad" class="value">—</span></div>
    <div id="adv-dbg-hint" style="margin-top:6px;font-size:10px;color:#69d195;display:none">
      Click anywhere or press a key first — Chrome locks the Gamepad API until then.
    </div>
  `;
  document.body.appendChild(dbg);

  // Toggle button + keyboard shortcut + ?debug=1 URL param.
  const toggle = document.createElement('div');
  toggle.id = 'adv-debug-toggle';
  toggle.textContent = '🔍';
  toggle.title = 'Toggle input debug (` key)';
  document.body.appendChild(toggle);
  function setDebugVisible(v) {
    dbg.classList.toggle('show', v);
    toggle.classList.toggle('active', v);
    toggle.classList.toggle('shifted', v);
  }
  toggle.addEventListener('click', () => setDebugVisible(!dbg.classList.contains('show')));
  document.addEventListener('keydown', (e) => {
    if (e.key === '`' && !isTypingTarget(e.target)) {
      e.preventDefault();
      setDebugVisible(!dbg.classList.contains('show'));
    }
  });
  if (new URLSearchParams(location.search).get('debug') === '1') {
    setDebugVisible(true);
  }

  function dbgSet(elId, txt) {
    const el = document.getElementById(elId);
    if (!el) return;
    el.textContent = txt;
    el.className = 'value';
    setTimeout(() => { if (el.textContent === txt) el.className = 'stale'; }, 2000);
  }
  function dbgSetPersistent(elId, txt) {
    const el = document.getElementById(elId);
    if (!el) return;
    el.textContent = txt;
    el.className = 'value';
  }
  // ─── server-side log mirror ───
  // The dev (me) needs to see what reaches the page without reading it off
  // a TV. Every interesting event also POSTs to /api/debug/input — the
  // server logs it via tracing so `docker logs adventurer-live` shows the
  // full picture. Fire-and-forget: a fetch with keepalive so we don't block.
  function logToServer(payload) {
    try {
      fetch('/api/debug/input', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
        keepalive: true,
      }).catch(() => {});
    } catch {}
  }
  // Throttle mouse moves to once per 250 ms so we don't spam.
  let lastMouseLog = 0;

  document.addEventListener('keydown', (e) => {
    dbgSet('adv-dbg-key', `${e.key} (${e.code})`);
    logToServer({ type: 'key', key: e.key, code: e.code, alt: e.altKey, ctrl: e.ctrlKey, shift: e.shiftKey });
  }, true);
  document.addEventListener('mousedown', (e) => {
    dbgSet('adv-dbg-mouse', `click @ ${e.clientX},${e.clientY}`);
    logToServer({ type: 'mouse_click', button: e.button, x: e.clientX, y: e.clientY });
  }, true);
  document.addEventListener('mousemove', (e) => {
    dbgSet('adv-dbg-mouse', `move @ ${e.clientX},${e.clientY}`);
    const now = performance.now();
    if (now - lastMouseLog > 250) {
      lastMouseLog = now;
      logToServer({ type: 'mouse_move', x: e.clientX, y: e.clientY });
    }
  }, { passive: true, capture: true });

  // One-shot startup banner so we know what page this is.
  logToServer({
    type: 'page_loaded',
    href: location.href,
    ua: navigator.userAgent,
    ratio: window.devicePixelRatio,
    pads_at_load: (navigator.getGamepads ? navigator.getGamepads().filter(p => p).length : 0),
  });

  // Live gamepad device count, regardless of button activity. Updates every
  // frame so we can tell whether Chrome sees the device at all.
  let lastPadCount = -1;
  let userGestured = false;
  function recordGesture() {
    if (!userGestured) {
      userGestured = true;
      const hint = document.getElementById('adv-dbg-hint');
      if (hint) hint.style.display = 'none';
    }
  }
  document.addEventListener('click', recordGesture, true);
  document.addEventListener('keydown', recordGesture, true);
  function watchPadCount() {
    const pads = (navigator.getGamepads ? navigator.getGamepads() : []) || [];
    const real = Array.from(pads).filter(p => p);
    if (real.length !== lastPadCount) {
      lastPadCount = real.length;
      if (real.length === 0) {
        dbgSetPersistent('adv-dbg-pads', userGestured ? '0 (none detected)' : '0 (waiting for gesture)');
        const hint = document.getElementById('adv-dbg-hint');
        if (hint && !userGestured) hint.style.display = '';
      } else {
        const ids = real.map(p => `${p.index}:${(p.id || '?').slice(0, 24)}`).join(', ');
        dbgSetPersistent('adv-dbg-pads', `${real.length} → ${ids}`);
      }
      logToServer({
        type: 'pad_count_change',
        count: real.length,
        gestured: userGestured,
        pads: real.map(p => ({
          idx: p.index, id: p.id, mapping: p.mapping,
          buttons: p.buttons.length, axes: p.axes.length,
          connected: p.connected,
        })),
      });
    }
    requestAnimationFrame(watchPadCount);
  }
  requestAnimationFrame(watchPadCount);

  // ─── focus management ───
  let focusEl = null;
  function setFocus(el) {
    if (focusEl && focusEl !== el) focusEl.classList.remove('adv-gp-focus');
    focusEl = el;
    if (focusEl) {
      focusEl.classList.add('adv-gp-focus');
      try { focusEl.focus({ preventScroll: false }); } catch {}
      try { focusEl.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); } catch {}
    }
  }
  function clearFocus() {
    if (focusEl) focusEl.classList.remove('adv-gp-focus');
    focusEl = null;
  }

  // Focusable selector. `<a>` with href is fine but we focus DM-relevant controls first.
  const FOCUS_SEL =
    'button:not([disabled]):not([hidden]),' +
    'input:not([type=hidden]):not([disabled]),' +
    'select:not([disabled]),' +
    'textarea:not([disabled]),' +
    'a[href]:not([hidden]),' +
    '[tabindex]:not([tabindex="-1"])';

  // Modal trap: when an overlay/modal is on screen, focus must stay inside
  // it. The dnd-stage UI uses a few naming conventions for full-screen
  // overlays — start-overlay, *-overlay, *-modal. We look for the topmost
  // visible one and restrict focus to its descendants. Without this, arrow
  // keys cycle through hidden buttons behind the overlay (e.g. header buttons
  // when the start screen is up).
  const MODAL_SELECTOR = [
    '#start-overlay',
    '#char-select-overlay',
    '#map-modal-overlay',
    '#modal-overlay',
    '#history-overlay',
    '#decision-overlay',
    '#panel-detail-overlay',
    '#end-session-overlay',
    '#adv-qr-overlay',
  ].join(',');

  function isVisible(el) {
    const cs = getComputedStyle(el);
    if (cs.display === 'none' || cs.visibility === 'hidden') return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }

  function activeModal() {
    const candidates = Array.from(document.querySelectorAll(MODAL_SELECTOR));
    // Among visible ones pick the highest z-index (topmost).
    let best = null, bestZ = -Infinity;
    for (const el of candidates) {
      if (!isVisible(el)) continue;
      const z = parseInt(getComputedStyle(el).zIndex, 10) || 0;
      if (z >= bestZ) { best = el; bestZ = z; }
    }
    return best;
  }

  function visibleFocusables() {
    const root = activeModal() || document.body;
    return Array.from(root.querySelectorAll(FOCUS_SEL))
      .filter(el => {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) return false;
        if (el.offsetParent === null && getComputedStyle(el).position !== 'fixed') return false;
        const cs = getComputedStyle(el);
        return cs.visibility !== 'hidden' && cs.display !== 'none';
      });
  }

  function step(direction) {
    const items = visibleFocusables();
    if (!items.length) return;
    let i = items.indexOf(focusEl);
    if (i === -1) {
      setFocus(items[0]);
      return;
    }
    i = (i + direction + items.length) % items.length;
    setFocus(items[i]);
  }

  // Mouse activity hides the focus ring; controller activity shows it.
  document.addEventListener('mousedown', () => {
    document.body.classList.remove('adv-gp-active');
    clearFocus();
  });

  // Auto-focus the first button when an overlay/modal appears — so a single
  // press of A (= Space in Steam KB+M) works without having to D-pad first.
  // We poll the active modal each frame; when it changes, retarget focus.
  let lastActiveModalId = '';
  function watchModalForAutoFocus() {
    const modal = activeModal();
    const id = modal ? modal.id : '';
    if (id !== lastActiveModalId) {
      lastActiveModalId = id;
      if (modal) {
        const items = visibleFocusables();
        if (items.length) {
          // Brief delay so DOM has settled (animations etc.).
          setTimeout(() => {
            const fresh = visibleFocusables();
            if (fresh.length) setFocus(fresh[0]);
          }, 50);
        }
      } else {
        clearFocus();
      }
    }
    requestAnimationFrame(watchModalForAutoFocus);
  }
  requestAnimationFrame(watchModalForAutoFocus);

  // ─── gamepad poll loop ───
  // Standard W3C button indices.
  const B = { A: 0, B: 1, X: 2, Y: 3, LB: 4, RB: 5,
              SELECT: 8, START: 9, UP: 12, DOWN: 13, LEFT: 14, RIGHT: 15 };
  const STICK_THRESHOLD = 0.55;

  let prevButtons = [];
  let prevAxes = { lx: 0, ly: 0 };
  let stickRepeatTimer = 0;

  function pressed(index, currentButtons) {
    return currentButtons[index] && !prevButtons[index];
  }
  function held(index, currentButtons) {
    return currentButtons[index];
  }

  function emitKey(key) {
    // Simulate a keydown — lets stage.js's existing hotkeys (R, U) work
    // unmodified.
    document.dispatchEvent(new KeyboardEvent('keydown', { key, code: key, bubbles: true }));
  }

  function clickFocused() {
    if (focusEl) {
      focusEl.click();
    }
  }

  function closeModal() {
    // dnd-stage and our overlays use various `*-overlay` divs. Find a visible
    // one and click its close button, or dispatch Escape.
    const open = Array.from(document.querySelectorAll(
      '[id$="-overlay"].show, [id$="-overlay"]:not(.hidden)'
    )).find(o => getComputedStyle(o).display !== 'none' && o.id !== 'start-overlay');
    if (open) {
      const close = open.querySelector('[id$="-close"], button.btn-modal-cancel');
      if (close) {
        close.click();
        return;
      }
    }
    emitKey('Escape');
  }

  function openPlayersModal() {
    const btn = document.getElementById('adv-players-btn');
    if (btn) btn.click();
  }

  function endSession() {
    const btn = document.getElementById('btn-end-session');
    if (btn) btn.click();
  }

  function poll() {
    const pads = navigator.getGamepads ? navigator.getGamepads() : [];
    let active = false;
    for (const gp of pads) {
      if (!gp) continue;
      active = true;
      const buttons = gp.buttons.map(b => typeof b === 'object' ? b.pressed : !!b);
      const ax = gp.axes;

      // First-time activation marker.
      if (!document.body.classList.contains('adv-gp-active')) {
        document.body.classList.add('adv-gp-active');
        status.classList.add('show');
        status.textContent = '🎮 ' + (gp.id || 'controller').slice(0, 30);
        if (!focusEl) {
          // Auto-focus the first sensible thing on first controller activity.
          const items = visibleFocusables();
          if (items.length) setFocus(items[0]);
        }
      }

      // ─── debug: any button transition → display + log ───
      for (let i = 0; i < buttons.length; i++) {
        if (buttons[i] && !prevButtons[i]) {
          dbgSet('adv-dbg-pad', `b${i} (${gp.id || 'gamepad'})`);
          logToServer({
            type: 'pad_button',
            pad_id: gp.id,
            pad_index: gp.index,
            button: i,
          });
        }
      }
      // Stick deflection logged once per significant transition.
      const lx = ax[0] || 0, ly = ax[1] || 0;
      if (Math.abs(lx) > STICK_THRESHOLD && !(Math.abs(prevAxes.lx) > STICK_THRESHOLD)) {
        logToServer({ type: 'pad_axis', axis: 'lx', value: lx });
      }
      if (Math.abs(ly) > STICK_THRESHOLD && !(Math.abs(prevAxes.ly) > STICK_THRESHOLD)) {
        logToServer({ type: 'pad_axis', axis: 'ly', value: ly });
      }

      // ─── face buttons ───
      if (pressed(B.A, buttons)) clickFocused();
      if (pressed(B.B, buttons)) closeModal();
      if (pressed(B.X, buttons)) emitKey('u');         // Update
      if (pressed(B.Y, buttons)) emitKey('r');         // Toggle recording
      if (pressed(B.START, buttons))  openPlayersModal();
      if (pressed(B.SELECT, buttons)) endSession();

      // ─── shoulders: faster nav ───
      if (pressed(B.LB, buttons)) step(-1);
      if (pressed(B.RB, buttons)) step(+1);

      // ─── d-pad ───
      if (pressed(B.LEFT,  buttons) || pressed(B.UP,    buttons)) step(-1);
      if (pressed(B.RIGHT, buttons) || pressed(B.DOWN,  buttons)) step(+1);

      // ─── left stick (debounced repeat at 200ms) — lx/ly already declared above ───
      const now = performance.now();
      const wantPrev = lx < -STICK_THRESHOLD || ly < -STICK_THRESHOLD;
      const wantNext = lx >  STICK_THRESHOLD || ly >  STICK_THRESHOLD;
      const wasPrev  = prevAxes.lx < -STICK_THRESHOLD || prevAxes.ly < -STICK_THRESHOLD;
      const wasNext  = prevAxes.lx >  STICK_THRESHOLD || prevAxes.ly >  STICK_THRESHOLD;
      if ((wantPrev && !wasPrev) || (wantPrev && now - stickRepeatTimer > 200)) {
        step(-1);
        stickRepeatTimer = now;
      }
      if ((wantNext && !wasNext) || (wantNext && now - stickRepeatTimer > 200)) {
        step(+1);
        stickRepeatTimer = now;
      }

      prevButtons = buttons;
      prevAxes = { lx, ly };
    }
    if (!active) {
      // Drop the focus ring + state if no gamepads attached.
      if (document.body.classList.contains('adv-gp-active')) {
        document.body.classList.remove('adv-gp-active');
        status.classList.remove('show');
        clearFocus();
      }
    }
    requestAnimationFrame(poll);
  }

  window.addEventListener('gamepadconnected', (e) => {
    console.log('[gamepad] connected:', e.gamepad.id, 'idx', e.gamepad.index);
  });
  window.addEventListener('gamepaddisconnected', (e) => {
    console.log('[gamepad] disconnected:', e.gamepad.id);
  });
  // Always poll — cheap, and catches connections that fire before our listener
  // (Chrome sometimes only fires gamepadconnected after the first input).
  requestAnimationFrame(poll);

  // ─── Path 2: keyboard nav (Steam Input feeds these from the controller) ───
  //
  // We bind on document with capture phase so we can navigate even when text
  // inputs have focus (we won't preventDefault for the input though — typing
  // still works because we let the event bubble for non-arrow keys).
  function isTypingTarget(t) {
    if (!t) return false;
    const tag = (t.tagName || '').toLowerCase();
    if (tag === 'input' || tag === 'textarea') return true;
    if (t.isContentEditable) return true;
    return false;
  }

  document.addEventListener('keydown', (e) => {
    // Steam's "Keyboard and Mouse" desktop template sends WASD for D-pad /
    // left stick and Space for the A button — not arrow keys / Enter as I
    // initially expected. Support both so we cover both templates.
    const k = e.key;
    const navKeys = new Set([
      'ArrowUp','ArrowDown','ArrowLeft','ArrowRight','Enter','Tab',
      'w','a','s','d','W','A','S','D',' ',
    ]);
    if (navKeys.has(k)) {
      document.body.classList.add('adv-gp-active');
      status.classList.add('show');
      status.textContent = '⌨ controller (via Steam Input)';
    }

    // If the user is typing in an input, only intercept Tab (native anyway).
    // Arrow keys / WASD / Space belong to the cursor + text.
    if (isTypingTarget(e.target)) return;

    switch (k) {
      case 'ArrowDown':
      case 'ArrowRight':
      case 's': case 'S':
      case 'd': case 'D':
        e.preventDefault();
        step(+1);
        break;
      case 'ArrowUp':
      case 'ArrowLeft':
      case 'w': case 'W':
      case 'a': case 'A':
        e.preventDefault();
        step(-1);
        break;
      case 'Enter':
      case ' ':              // Space — Steam KB+M sends this for A button
        // If a button is gamepad-focused, click it.
        if (focusEl) {
          e.preventDefault();
          clickFocused();
        }
        break;
      case 'Escape':
        e.preventDefault();
        closeModal();
        break;
    }
  }, true);
})();
