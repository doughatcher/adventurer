// Adventure Log — transcript ambient-sound styling + filter.
//
// The vendored dnd-stage UI renders every transcript line the same way.
// Whisper sometimes captures real ambient sounds in brackets (e.g.
// `[paper rustling]`, `[footsteps]`, `(scissors cutting)`) — those carry
// useful scene atmosphere but visually clutter the speech log.
//
// This script (additive, no changes to stage.js):
//   1. MutationObserver watches the transcript area for new lines
//   2. Lines whose ENTIRE non-timestamp content is a bracketed/parenthesized
//      sound description get wrapped/marked with class `adv-ambient`
//   3. CSS dims them, italicizes, and prefixes with a 🔊 icon
//   4. A new filter button "🔊" in the existing `.log-filters` row
//      toggles their visibility (mirrors the existing tx-noise / etc filters)

(() => {
  if (window.__advTranscriptStyleInit) return;
  window.__advTranscriptStyleInit = true;

  // ─── styles ───
  const css = document.createElement('style');
  css.textContent = `
    .adv-ambient {
      font-style: italic;
      color: var(--text3, #5b6470) !important;
      opacity: .85;
    }
    .adv-ambient::before {
      content: "🔊 ";
      font-style: normal;
      opacity: .8;
      margin-right: 1px;
    }
    .adv-ambient.adv-paren::before { content: "💨 "; } /* parens often = subtler */
    body.adv-hide-ambient .adv-ambient { display: none !important; }

    /* Filter button visual to match the existing .log-filters style */
    #adv-ambient-filter-btn {
      cursor: pointer;
      font-size: 12px;
      padding: 2px 6px;
      border-radius: 4px;
      background: transparent;
      color: var(--text2, #8d96a7);
      border: 1px solid var(--line, #2a313c);
      margin-left: 4px;
    }
    #adv-ambient-filter-btn.active {
      background: rgba(244,185,66,.15);
      color: var(--accent, #f4b942);
      border-color: var(--accent, #f4b942);
    }
  `;
  document.head.appendChild(css);

  // Patterns that match Whisper's ambient-sound descriptions. We accept BOTH
  // square brackets AND parens because both show up in practice
  // (whisper.cpp formats vary by model/version/language).
  const SQUARE  = /^\s*\[([^\[\]\n]+)\]\s*$/;
  const PAREN   = /^\s*\(([^()\n]+)\)\s*$/;

  // The dnd-stage UI prefixes each line with `**[HH:MM:SS]**` — that "[…]"
  // is a TIMESTAMP, not a sound annotation. We strip it before checking.
  // (After it's been rendered, the timestamp lives in its own span — but
  // when scanning text we may encounter it inline.)
  const TIMESTAMP_LEAD = /^\s*(\*\*)?\[\d{1,2}:\d{2}(:\d{2})?\](\*\*)?\s*/;

  function classifyBody(text) {
    // strip a possible "[HH:MM:SS]" timestamp prefix
    const body = text.replace(TIMESTAMP_LEAD, '').trim();
    if (!body) return null;
    if (SQUARE.test(body)) return 'square';
    if (PAREN.test(body))  return 'paren';
    return null;
  }

  function maybeMarkLine(el) {
    if (!el || el.dataset.advAmbientChecked === '1') return;
    el.dataset.advAmbientChecked = '1';
    const text = (el.textContent || '');
    const kind = classifyBody(text);
    if (kind) {
      el.classList.add('adv-ambient');
      if (kind === 'paren') el.classList.add('adv-paren');
    }
  }

  // The dnd-stage transcript container is `#log-body`. Each entry is a
  // direct child element (could be `div`, `p`, or just text nodes — we
  // observe everything and check.)
  function scan(root) {
    if (!root) return;
    // Direct children — speech lines and the like
    root.querySelectorAll('[id^="log-"], [class*="tx-"], div, p, span, li').forEach(maybeMarkLine);
  }

  function attachObserver() {
    const log = document.getElementById('log-body');
    if (!log) {
      // Try again later — stage.js may build the log dynamically
      setTimeout(attachObserver, 500);
      return;
    }
    scan(log);
    const mo = new MutationObserver((records) => {
      for (const rec of records) {
        for (const node of rec.addedNodes) {
          if (node.nodeType !== 1) continue;
          maybeMarkLine(node);
          scan(node);
        }
      }
    });
    mo.observe(log, { childList: true, subtree: true });
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', attachObserver);
  } else {
    attachObserver();
  }

  // ─── filter button ───
  function injectFilter() {
    const filters = document.getElementById('log-filters');
    if (!filters || document.getElementById('adv-ambient-filter-btn')) return;
    const btn = document.createElement('button');
    btn.id = 'adv-ambient-filter-btn';
    btn.className = 'filter-btn active';
    btn.title = 'Show / hide ambient sound captures';
    btn.innerHTML = '🔊';
    btn.addEventListener('click', () => {
      const hide = !document.body.classList.contains('adv-hide-ambient');
      document.body.classList.toggle('adv-hide-ambient', hide);
      btn.classList.toggle('active', !hide);
    });
    filters.appendChild(btn);
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', injectFilter);
  } else {
    injectFilter();
  }
  // Re-inject if stage.js rebuilds the header.
  setInterval(injectFilter, 2000);
})();
