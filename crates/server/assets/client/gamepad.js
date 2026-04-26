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

  function visibleFocusables() {
    return Array.from(document.querySelectorAll(FOCUS_SEL))
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

      // ─── left stick (debounced repeat at 200ms) ───
      const lx = ax[0] || 0, ly = ax[1] || 0;
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
    // While the controller is "active" we add the body class so the focus
    // ring is visible. Any meaningful key press counts.
    const navKeys = ['ArrowUp','ArrowDown','ArrowLeft','ArrowRight','Enter','Tab'];
    if (navKeys.includes(e.key)) {
      document.body.classList.add('adv-gp-active');
      status.classList.add('show');
      status.textContent = '⌨ controller (via Steam Input)';
    }

    // If the user is typing in an input, only intercept Tab (which is native
    // anyway). Arrow keys belong to the cursor.
    if (isTypingTarget(e.target)) return;

    switch (e.key) {
      case 'ArrowDown':
      case 'ArrowRight':
        e.preventDefault();
        step(+1);
        break;
      case 'ArrowUp':
      case 'ArrowLeft':
        e.preventDefault();
        step(-1);
        break;
      case 'Enter':
        // If a button is gamepad-focused, click it. (Native Enter on a button
        // already triggers click, but we want the same flow for divs/inputs.)
        if (focusEl && focusEl.tagName !== 'BUTTON') {
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
