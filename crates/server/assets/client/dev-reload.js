// Adventure Log — dev-mode live reload.
//
// When the server is launched with ADVENTURER_DEV=1 (mounting the host
// `assets/` dir), it spawns a `notify` watcher; on any file change the
// server emits a `dev_reload` event over /ws.
//
// This script subscribes to /ws, on receiving `dev_reload` it
// `location.reload()`s the page. In production builds (no watcher) the
// event never fires, so this is harmless.

(() => {
  if (window.__advDevReloadInit) return;
  window.__advDevReloadInit = true;

  let ws = null;
  let backoff = 1000;

  function connect() {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    try {
      ws = new WebSocket(`${proto}//${location.host}/ws?role=dev-reload`);
    } catch { setTimeout(connect, 5000); return; }
    ws.addEventListener('open', () => { backoff = 1000; });
    ws.addEventListener('close', () => {
      setTimeout(connect, backoff);
      backoff = Math.min(backoff * 2, 15000);
    });
    ws.addEventListener('error', () => {/* close handler retries */});
    ws.addEventListener('message', (ev) => {
      let msg; try { msg = JSON.parse(ev.data); } catch { return; }
      if (msg && msg.type === 'dev_reload') {
        // Tiny visual nudge so we know it's intentional, then reload.
        try {
          const tag = document.createElement('div');
          tag.textContent = '↻ asset changed — reloading';
          tag.style.cssText = 'position:fixed;top:8px;left:50%;transform:translateX(-50%);'
            + 'background:#63a8e8;color:#0e1116;padding:6px 14px;border-radius:999px;'
            + 'font-family:ui-monospace,monospace;font-size:12px;font-weight:600;'
            + 'z-index:99999;box-shadow:0 2px 12px rgba(0,0,0,.5)';
          document.body.appendChild(tag);
        } catch {}
        setTimeout(() => location.reload(), 120);
      }
    });
  }
  connect();
})();
