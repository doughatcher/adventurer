// DnD Stage frontend

// ── Minimal markdown renderer (no external deps) ──
function renderMd(text) {
  if (!text) return '';
  // Remove ## PANEL: header lines
  text = text.replace(/^## PANEL:.*$/mg, '').trim();

  const lines = text.split('\n');
  let html = '';
  let inUl = false;

  for (let line of lines) {
    // Headings
    if (/^### (.+)/.test(line)) {
      if (inUl) { html += '</ul>'; inUl = false; }
      html += `<h3>${md_inline(line.slice(4))}</h3>`;
    } else if (/^## (.+)/.test(line)) {
      if (inUl) { html += '</ul>'; inUl = false; }
      html += `<h2>${md_inline(line.slice(3))}</h2>`;
    } else if (/^# (.+)/.test(line)) {
      if (inUl) { html += '</ul>'; inUl = false; }
      html += `<h1>${md_inline(line.slice(2))}</h1>`;
    }
    // HR
    else if (/^---+$/.test(line.trim())) {
      if (inUl) { html += '</ul>'; inUl = false; }
      html += '<hr>';
    }
    // List items
    else if (/^[-*] (.+)/.test(line)) {
      if (!inUl) { html += '<ul>'; inUl = true; }
      html += `<li>${md_inline(line.slice(2))}</li>`;
    }
    // Blank
    else if (line.trim() === '') {
      if (inUl) { html += '</ul>'; inUl = false; }
    }
    // Paragraph
    else {
      if (inUl) { html += '</ul>'; inUl = false; }
      html += `<p>${md_inline(line)}</p>`;
    }
  }
  if (inUl) html += '</ul>';
  return html;
}

function md_inline(text) {
  return text
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/\*(.+?)\*/g, '<em>$1</em>')
    .replace(/`(.+?)`/g, '<code>$1</code>');
}

function escHtml(s) {
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}

// ── Panel map (scene, map, next-steps only) ──
const PANELS = {
  'scene':      { el: () => document.getElementById('panel-scene-body'),   wrap: () => document.getElementById('panel-scene') },
  'next-steps': { el: () => document.getElementById('panel-next-body'),    wrap: () => document.getElementById('panel-next') },
  'map':        { el: () => document.getElementById('panel-map-body'),     wrap: () => document.getElementById('panel-map') },
};

let _lastSceneText = '';

function injectSceneBeat(content) {
  // Extract first meaningful sentence from scene content
  const cleaned = content
    .replace(/^## PANEL:.*$/mg, '')
    .replace(/^#+\s+/mg, '')
    .trim();
  if (!cleaned || cleaned === _lastSceneText) return;
  _lastSceneText = cleaned;

  // First sentence or up to 120 chars
  const sentence = cleaned.match(/^[^.!?]+[.!?]/)?.[0] || cleaned.slice(0, 120);
  if (!sentence.trim()) return;

  const logBody = document.getElementById('log-body');
  if (!logBody) return;
  const wasAtBottom = logBody.scrollHeight - logBody.scrollTop - logBody.clientHeight < 60;

  const div = document.createElement('div');
  div.className = 'log-scene-beat';
  div.innerHTML = `<span class="scene-icon">&#9672;</span><span class="scene-text">${escHtml(sentence.trim())}</span>`;
  logBody.appendChild(div);

  if (wasAtBottom) requestAnimationFrame(() => { logBody.scrollTop = logBody.scrollHeight; });
}

function renderScene(content) {
  const text = content.replace(/^## PANEL:.*$/mg, '').replace(/^#+\s+/mg, '').trim();
  if (!text) return '';
  const m = text.match(/^([^.!?\n]+[.!?]?)\n?([\s\S]*)$/);
  const headline = m ? m[1].trim() : text;
  const detail = m ? m[2].trim() : '';
  return `<p class="scene-headline">${md_inline(escHtml(headline))}</p>` +
    (detail ? `<p class="scene-detail">${md_inline(escHtml(detail))}</p>` : '');
}

let _lastMapContent = '';
let _cyInstance = null;
let _cyModalInstance = null;

function _parseMapData(content) {
  const nodes = [], edges = [], here = {};
  const lines = content.replace(/^## PANEL:.*$/mg, '').split('\n');
  for (const raw of lines) {
    const line = raw.trim();
    if (line.startsWith('node:')) {
      const parts = line.slice(5).split('|').map(s => s.trim());
      if (parts.length >= 2) nodes.push({ id: parts[0], label: parts[1], type: parts[2] || 'room' });
    } else if (line.startsWith('edge:')) {
      const parts = line.slice(5).split('|').map(s => s.trim());
      if (parts.length >= 2) edges.push({ from: parts[0], to: parts[1], label: parts[2] || '' });
    } else if (line.startsWith('here:')) {
      const parts = line.slice(5).split('|').map(s => s.trim());
      if (parts.length >= 2) here[parts[0]] = parts[1];
    }
  }
  return { nodes, edges, here };
}

const NODE_COLORS = {
  room: '#2a3a4a', dungeon: '#2a2030', building: '#2a3530',
  outdoors: '#1e3020', area: '#2a3a4a', water: '#1a2840', default: '#2a3040'
};

function _buildCytoscapeElements(data) {
  const charsByNode = {};
  for (const [char, nodeId] of Object.entries(data.here)) {
    if (!charsByNode[nodeId]) charsByNode[nodeId] = [];
    charsByNode[nodeId].push(char.split(' ')[0]);
  }
  const elements = [];
  for (const n of data.nodes) {
    const chars = charsByNode[n.id] || [];
    const badge = chars.length ? `\n[${chars.join(',')}]` : '';
    elements.push({ data: { id: n.id, label: n.label + badge, type: n.type, hasChars: chars.length > 0 } });
  }
  for (let i = 0; i < data.edges.length; i++) {
    elements.push({ data: { id: `e${i}`, source: data.edges[i].from, target: data.edges[i].to, label: data.edges[i].label } });
  }
  return elements;
}

const CY_STYLE = [
  {
    selector: 'node',
    style: {
      'background-color': (ele) => NODE_COLORS[ele.data('type')] || NODE_COLORS.default,
      'border-color': '#3a5060', 'border-width': 1,
      'label': 'data(label)', 'color': '#c8d8e0',
      'font-size': '10px', 'font-family': 'inherit',
      'text-wrap': 'wrap', 'text-max-width': '80px',
      'text-valign': 'center', 'text-halign': 'center',
      'width': 'label', 'height': 'label', 'padding': '8px',
      'shape': 'round-rectangle',
    }
  },
  { selector: 'node[?hasChars]', style: { 'border-color': '#5090b0', 'border-width': 2, 'background-color': '#1a3a50' } },
  {
    selector: 'edge',
    style: {
      'width': 1, 'line-color': '#3a5060',
      'target-arrow-color': '#3a5060', 'target-arrow-shape': 'none',
      'curve-style': 'bezier', 'label': 'data(label)',
      'font-size': '9px', 'color': '#6a8090',
      'text-background-color': '#141b22', 'text-background-opacity': 1, 'text-background-padding': '2px',
    }
  }
];

function renderMapCytoscape(data, el) {
  el.innerHTML = '<div class="cy-container" id="cy-map"></div>';
  const cyEl = el.querySelector('#cy-map');
  if (typeof window.cytoscape === 'undefined') return;
  if (_cyInstance) { _cyInstance.destroy(); _cyInstance = null; }
  _cyInstance = window.cytoscape({
    container: cyEl, elements: _buildCytoscapeElements(data), style: CY_STYLE,
    layout: { name: 'cose', padding: 20, animate: false, nodeRepulsion: 4000, idealEdgeLength: 80 },
    userZoomingEnabled: true, userPanningEnabled: true, boxSelectionEnabled: false, autoungrabify: true,
  });
}

function renderMapModal(data) {
  const cyEl = document.getElementById('map-modal-cy');
  if (!cyEl || typeof window.cytoscape === 'undefined') return;
  if (_cyModalInstance) { _cyModalInstance.destroy(); _cyModalInstance = null; }
  _cyModalInstance = window.cytoscape({
    container: cyEl, elements: _buildCytoscapeElements(data), style: CY_STYLE,
    layout: { name: 'cose', padding: 30, animate: false, nodeRepulsion: 5000, idealEdgeLength: 120 },
    userZoomingEnabled: true, userPanningEnabled: true, boxSelectionEnabled: false, autoungrabify: true,
  });
}

function renderMap(content, el) {
  _lastMapContent = content;
  const data = _parseMapData(content);
  // Fall back to text if no structured data found
  if (data.nodes.length === 0) {
    const stripped = content.replace(/^## PANEL:.*$/mg, '').trim();
    el.innerHTML = `<pre>${escHtml(stripped)}</pre>`;
    return;
  }
  renderMapCytoscape(data, el);
}

function setPanel(name, content) {
  const p = PANELS[name];
  if (!p) return;
  const el = p.el();
  const wrap = p.wrap();
  if (!el || !content) return;
  if (name === 'scene') {
    injectSceneBeat(content);
    el.innerHTML = renderScene(content);
  } else if (name === 'map') {
    renderMap(content, el);
  } else {
    el.innerHTML = renderMd(content);
  }
  wrap.classList.remove('updated');
  void wrap.offsetWidth;
  wrap.classList.add('updated');
}

// ── Map modal ──

function openMapModal() {
  const overlay = document.getElementById('map-modal-overlay');
  // Populate raw data
  const asciiEl = document.getElementById('map-modal-ascii');
  asciiEl.textContent = _lastMapContent.replace(/^## PANEL:.*$/mg, '').trim() || '(No map data yet)';
  // Restore saved URL/cookie inputs
  const savedUrl = localStorage.getItem('ddb-map-url') || '';
  const savedCookie = localStorage.getItem('ddb-cookie') || '';
  document.getElementById('ddb-url-input').value = savedUrl;
  document.getElementById('ddb-cookie-input').value = savedCookie;
  // Render graph in modal
  const mapData = _parseMapData(_lastMapContent);
  if (mapData.nodes.length > 0) {
    renderMapModal(mapData);
  } else {
    const cyEl = document.getElementById('map-modal-cy');
    if (cyEl) cyEl.innerHTML = '<p style="color:var(--text3);padding:20px;font-size:12px">No map data yet — start session and wait for an AI update.</p>';
  }
  // Default to VTT tab if frame loaded, else graph
  const frame = document.getElementById('ddb-vtt-frame');
  switchMapTab(frame.classList.contains('loaded') ? 'vtt' : 'graph');
  overlay.classList.add('open');
}

async function loadDdbMap() {
  const url = document.getElementById('ddb-url-input').value.trim();
  const cookie = document.getElementById('ddb-cookie-input').value.trim();
  if (!url) return;
  localStorage.setItem('ddb-map-url', url);
  if (cookie) localStorage.setItem('ddb-cookie', cookie);

  // Send cookie to server proxy
  if (cookie) {
    await fetch('/api/ddb-cookie', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({cookie}),
    });
  }

  const frame = document.getElementById('ddb-vtt-frame');
  const btn = document.getElementById('ddb-load-btn');
  btn.textContent = 'Loading…';
  btn.disabled = true;
  frame.classList.remove('loaded');
  frame.src = `/api/proxy?url=${encodeURIComponent(url)}`;
  frame.onload = () => {
    frame.classList.add('loaded');
    btn.textContent = 'Reload';
    btn.disabled = false;
  };
  frame.onerror = () => {
    btn.textContent = 'Load Map';
    btn.disabled = false;
  };
  switchMapTab('vtt');
}

function closeMapModal() {
  document.getElementById('map-modal-overlay').classList.remove('open');
}

function switchMapTab(tab) {
  document.getElementById('map-tab-vtt').classList.toggle('hidden', tab !== 'vtt');
  document.getElementById('map-tab-graph').classList.toggle('hidden', tab !== 'graph');
  document.querySelectorAll('.map-tab').forEach(btn => {
    btn.classList.toggle('active', btn.dataset.tab === tab);
  });
  // Fit graph to container after it becomes visible
  if (tab === 'graph' && _cyModalInstance) {
    requestAnimationFrame(() => _cyModalInstance.fit(undefined, 30));
  }
}

// ── Character selection ──
let currentCharacter = localStorage.getItem('dnd-stage-character') || null;

async function showCharSelectOverlay() {
  const overlay = document.getElementById('char-select-overlay');
  const list = document.getElementById('char-select-list');
  list.innerHTML = '';
  try {
    const chars = await fetch('/api/characters').then(r => r.json());
    if (chars.length === 0) {
      list.innerHTML = '<p style="color:var(--text3);font-size:12px">No characters yet — add one after loading.</p>';
    } else {
      for (const c of chars) {
        const name = c.name || 'Unknown';
        const cls = c.class || '';
        const btn = document.createElement('button');
        btn.className = 'char-select-btn';
        btn.innerHTML = `${escHtml(name)}${cls ? `<span class="char-select-class">${escHtml(cls)}</span>` : ''}`;
        btn.addEventListener('click', () => {
          currentCharacter = name;
          localStorage.setItem('dnd-stage-character', name);
          hideCharSelectOverlay();
        });
        list.appendChild(btn);
      }
    }
  } catch(e) {
    list.innerHTML = '<p style="color:var(--text3);font-size:12px">Could not load characters.</p>';
  }
  overlay.classList.add('open');
}

function hideCharSelectOverlay() {
  document.getElementById('char-select-overlay').classList.remove('open');
}

// ── Party cards ──
function renderPartyCards(stateOrChars) {
  const container = document.getElementById('party-cards');
  if (!container) return;
  container.innerHTML = '';

  let characters = {};

  if (Array.isArray(stateOrChars)) {
    for (const c of stateOrChars) {
      const name = c.name;
      if (!name) continue;
      const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
      characters[slug] = {
        name, class: c.class || '',
        hp: parseInt(c.hp_current) || null,
        max_hp: parseInt(c.hp_max) || null,
        ac: parseInt(c.ac) || null,
        conditions: [], notes: c.notes || '',
        is_enemy: false, status: 'alive',
      };
    }
  } else if (stateOrChars && typeof stateOrChars === 'object') {
    characters = stateOrChars;
  }

  const entries = Object.entries(characters).filter(([, c]) => c.status !== 'dead');
  if (entries.length === 0) {
    container.innerHTML = '<p style="color:var(--text3);font-size:11px;padding:8px 4px">No characters yet.<br>Click + to add one.</p>';
    return;
  }

  const party   = entries.filter(([, c]) => !c.is_enemy);
  const enemies = entries.filter(([, c]) =>  c.is_enemy);

  const makeCard = ([slug, char], isEnemy) => {
    const name = char.name || slug;
    const cls = char.class || '';
    const hp = char.hp != null ? parseInt(char.hp) : null;
    const maxHp = char.max_hp != null ? parseInt(char.max_hp) : null;
    const ac = char.ac != null ? parseInt(char.ac) : null;
    const conditions = char.conditions || [];
    const isUnconscious = char.status === 'unconscious';

    let hpBarHtml = '';
    if (hp !== null && maxHp !== null && maxHp > 0) {
      const pct = Math.max(0, Math.min(100, (hp / maxHp) * 100));
      const hpClass = isEnemy ? 'hp-enemy' : (pct > 60 ? 'hp-high' : pct > 30 ? 'hp-mid' : 'hp-low');
      hpBarHtml = `<div class="hp-bar"><div class="hp-fill ${hpClass}" style="width:${pct}%"></div></div>`;
    } else {
      // Unknown HP — show a dim bar so the card doesn't look empty
      hpBarHtml = `<div class="hp-bar"><div class="hp-fill hp-unknown"></div></div>`;
    }

    let hpText = hp !== null && maxHp !== null && maxHp > 0 ? `${hp} / ${maxHp}`
               : hp !== null && hp > 0 ? `HP ${hp}`
               : maxHp !== null && maxHp > 0 ? `? / ${maxHp}`
               : '? HP';

    const acHtml = ac !== null ? `<span class="char-card-ac">AC ${ac}</span>` : '';
    const condHtml = conditions.length > 0
      ? `<div class="char-conditions">${conditions.map(c => `<span class="condition-chip">${escHtml(c)}</span>`).join('')}</div>`
      : '';
    const isActive = !isEnemy && currentCharacter && currentCharacter.toLowerCase() === name.toLowerCase();

    const card = document.createElement('div');
    card.className = `char-card${isEnemy ? ' enemy-card' : ''}${isActive ? ' active-char' : ''}${isUnconscious ? ' unconscious' : ''}`;
    card.dataset.slug = slug;
    card.innerHTML = `
      <div class="char-card-name">${escHtml(name)}</div>
      ${cls ? `<div class="char-card-class">${escHtml(cls)}</div>` : ''}
      ${hpBarHtml}
      <div class="char-card-meta">
        ${hpText ? `<span class="char-card-hp">${hpText}</span>` : ''}
        ${acHtml}
      </div>
      ${condHtml}
    `;
    if (!isEnemy) {
      card.addEventListener('click', () => {
        editingSlug = slug;
        document.getElementById('modal-title').textContent = 'Edit Character';
        document.getElementById('char-name').value = name;
        document.getElementById('char-class').value = cls;
        document.getElementById('char-hp-cur').value = hp !== null ? hp : '';
        document.getElementById('char-hp-max').value = maxHp !== null ? maxHp : '';
        document.getElementById('char-ac').value = ac !== null ? ac : '';
        document.getElementById('char-notes').value = char.notes || '';
        document.getElementById('modal-overlay').classList.add('open');
        document.getElementById('char-name').focus();
      });
    }
    return card;
  };

  party.forEach(e => container.appendChild(makeCard(e, false)));

  if (enemies.length > 0) {
    const divider = document.createElement('div');
    divider.className = 'party-divider';
    divider.textContent = '⚔ Enemies';
    container.appendChild(divider);
    enemies.forEach(e => container.appendChild(makeCard(e, true)));
  }
}

// ── Story beats in log ──
let _lastStoryBeatCount = 0;

function syncStoryBeats(content) {
  if (!content) return;
  // Extract bullet points from story-log content
  const beatRe = /^[-*] (.+)/mg;
  const beats = [];
  let m;
  const stripped = content.replace(/^## PANEL:.*$/mg, '');
  while ((m = beatRe.exec(stripped)) !== null) {
    beats.push(m[1].trim());
  }
  if (beats.length <= _lastStoryBeatCount) return;

  const logBody = document.getElementById('log-body');
  if (!logBody) return;

  const wasAtBottom = logBody.scrollHeight - logBody.scrollTop - logBody.clientHeight < 60;

  for (let i = _lastStoryBeatCount; i < beats.length; i++) {
    const div = document.createElement('div');
    div.className = 'log-beat';
    div.innerHTML = md_inline(escHtml(beats[i]));
    logBody.appendChild(div);
  }
  _lastStoryBeatCount = beats.length;

  if (wasAtBottom) {
    requestAnimationFrame(() => { logBody.scrollTop = logBody.scrollHeight; });
  }
}

// ── Transcript rendering into log ──
let _lastTranscriptLineCount = 0;

// ── Transcript line classifier ──
// Categories: tx-dm | tx-player | tx-roll | tx-meta | tx-noise

function classifyLine(text) {
  const t = text.trim();
  const lo = t.toLowerCase();
  const words = t.split(/\s+/).filter(Boolean).length;

  // ── Noise: filler, very short with no game info ──
  if (words <= 2 && !/\d/.test(t)) return 'tx-noise';
  if (/^(yeah[,.]?|yep|nope|okay[,.]?|ok[,.]?|uh+|um+|hmm+|huh|oh|ah|ow|wait|thanks|alright|right right|sure|cool|nice|oh no|oh wow|wow)\.?$/i.test(t)) return 'tx-noise';
  if (words <= 4 && /^(hold on|one sec|hang on|never mind|nevermind|you know|i know|i see|got it|got you|sounds good|fair enough|makes sense)$/i.test(lo)) return 'tx-noise';

  // ── Roll / mechanical: dice results, numbers, skill checks ──
  if (/\b(nat(ural)?\s*(20|1|twenty|one)|critical hit|crit(ical)?|fumble|auto.?hit|auto.?miss)\b/i.test(t)) return 'tx-roll';
  if (/\b(rolled?|rolls?)\s+(a\s+)?\d+\b/i.test(lo)) return 'tx-roll';
  if (/\bthat'?s?\s+(a\s+)?\d+\b/i.test(lo) && words < 12) return 'tx-roll';
  if (/^\+?\d+[,.]?\s*$/.test(t)) return 'tx-roll';
  if (/\b(to hit|for damage|saving throw|death save|spell save|concentration check|con(stitution)? save)\b/i.test(lo)) return 'tx-roll';
  if (/\b(initiative|perception|insight|stealth|athletics|acrobatics|arcana|history|investigation|persuasion|deception|intimidation|medicine|survival|religion|nature|sleight of hand|animal handling)\s*(check|roll|result|score)?\b/i.test(lo) && words < 12) return 'tx-roll';
  if (/\b(missed|hit|crits?|whiffs?)\b/i.test(lo) && words < 8) return 'tx-roll';

  // ── Meta / OOC: rules, game mechanics, table talk ──
  if (/\b(d&?d|dnd|beyond|player.?s? handbook|dungeon master.?s? guide|monster manual|homebrew|house rule|rules? as written|r\.?a\.?w\.?|errata|sage advice|page \d+|chapter \d+)\b/i.test(lo)) return 'tx-meta';
  if (/\b(can (that|this|it|you|i) (stack|apply|work|count|trigger|proc)|does (that|this|the) (work|apply|count|stack|trigger)|is (that|this) (allowed|legal|correct|right|how it works)|technically|according to (the )?(rules?|phb|dmg)|rule of|feature says|it says|the (spell|feat|ability|feature) says)\b/i.test(lo)) return 'tx-meta';
  if (/\b(advantage|disadvantage)\s+(on|for|to)\b/i.test(lo) && words < 10) return 'tx-meta';
  if (/\b(bonus action|reaction|free action|action economy|action surge|second wind|bardic inspiration|ki point|spell slot|sorcery point|superiority die|channel divinity)\b/i.test(lo) && words < 10) return 'tx-meta';
  // OOC planning / shopping / downtime discussion
  if (/\b(what should (i|we) (buy|get|pick|take|do)|what do (you|they) have|how much (does|is|do)|can (i|we) buy|i('d| would) like to (buy|get|purchase)|do (you|they) sell|looking for|in the market|what('s| is) available|back at (camp|town|base)|during (downtime|the rest))\b/i.test(lo)) return 'tx-meta';
  if (/\b(next (session|time|week)|last (session|time|week)|remember when|real (life|world)|in real life|i r[- ]l|bathroom|snack|pizza|beer|bathroom break|be right back|brb)\b/i.test(lo)) return 'tx-meta';

  // ── Player: first-person action declarations (check before DM) ──
  if (/^(i |we |my |i'm |i'll |i've |i'd |i want |i try |i use |i cast |i move |i attack |i go |i grab |i draw |i pull |i run |i dash |i hide |i dodge |i help |i ready |i shove |i grapple)/i.test(t)) return 'tx-player';
  if (/\b(as (my|an) action|my (bonus action|reaction|turn|movement)|i spend|i expend|i use my)\b/i.test(lo)) return 'tx-player';
  // Conversational player speech (questions and reactions that start with first person)
  if (/^(can i |do i |will i |should i |would i |have i |am i |did i |does my )/i.test(t)) return 'tx-player';
  if (/^(can we |do we |will we |should we |would we |have we |are we |did we )/i.test(t)) return 'tx-player';

  // ── DM: narration, scene-setting, consequence delivery ──
  if (/\b(you (see|hear|feel|notice|find|discover|realize|arrive|enter|emerge|spot|detect)|before you|around you|ahead of you|in front of you)\b/i.test(lo)) return 'tx-dm';
  if (/\b(the (party|group|adventurers)|as you|when you|you are now|you have)\b/i.test(lo) && words > 6) return 'tx-dm';
  if (/\b(takes?|deal[st]?|inflicts?)\s+\d+\s+(points?\s+of\s+)?(damage|healing|hit\s*points?|hp)\b/i.test(lo)) return 'tx-dm';
  if (/\b(roll(ing)?\s+(for|a|your)|make\s+a\s+|give\s+me\s+a\s+)\b/i.test(lo)) return 'tx-dm';
  if (/\b(the\s+(creature|monster|enemy|goblin|orc|undead|skeleton|zombie|vampire|dragon|beast|fiend|celestial|demon|devil|npc|priest|guard|soldier|warrior|bandit|cultist))\b/i.test(lo)) return 'tx-dm';
  if (/\b(emerges?|attacks?|strikes?|charges?|retreats?|falls?\s+(prone|unconscious|dead)|drops?\s+(to\s+0|dead|unconscious))\b/i.test(lo) && words > 6) return 'tx-dm';

  // Default: first-person → player, third-person narrative → DM, other long → meta
  if (/^(i |we |my )/i.test(t)) return 'tx-player';
  if (words > 20) return 'tx-dm';
  return 'tx-player';
}

// ── Filter state ──
const _filters = { 'tx-dm': true, 'tx-player': true, 'tx-roll': true, 'tx-meta': true, 'tx-noise': false };

function applyFilter(cls) {
  const show = _filters[cls];
  document.querySelectorAll(`.log-transcript.${cls}`).forEach(el => {
    el.style.display = show ? '' : 'none';
  });
}

function toggleFilter(cls) {
  _filters[cls] = !_filters[cls];
  applyFilter(cls);
  // Update button state
  const btn = document.querySelector(`.filter-btn[data-cls="${cls}"]`);
  if (btn) btn.classList.toggle('active', _filters[cls]);
}

function renderTranscript(text) {
  if (!text) return;
  const logBody = document.getElementById('log-body');
  if (!logBody) return;
  const wasAtBottom = logBody.scrollHeight - logBody.scrollTop - logBody.clientHeight < 60;

  const lineRe = /\*\*\[(\d{2}:\d{2}:\d{2})\]\*\*\s*(.+)/g;
  const lines = [];
  let m;
  while ((m = lineRe.exec(text)) !== null) {
    lines.push({ time: m[1], text: m[2].trim() });
  }
  if (lines.length === 0) return;

  // Count existing transcript lines in log
  const existingTxLines = logBody.querySelectorAll('.log-transcript').length;
  if (existingTxLines > lines.length) {
    // Remove all transcript lines and re-add (stale state)
    logBody.querySelectorAll('.log-transcript').forEach(el => el.remove());
    _lastTranscriptLineCount = 0;
  }

  const start = Math.min(_lastTranscriptLineCount, lines.length);
  for (let i = start; i < lines.length; i++) {
    const { time, text } = lines[i];
    const cls = classifyLine(text);
    const div = document.createElement('div');
    div.className = `log-transcript ${cls}`;
    if (!_filters[cls]) div.style.display = 'none';
    const label = cls === 'tx-dm' ? 'DM' : cls === 'tx-roll' ? '⚄' : cls === 'tx-meta' ? '?' : cls === 'tx-noise' ? '…' : '';
    div.innerHTML = `<span class="tx-time">${time}</span>${label ? `<span class="tx-cat">${label}</span>` : ''}<span class="tx-text">${escHtml(text)}</span>`;
    logBody.appendChild(div);
  }
  _lastTranscriptLineCount = lines.length;

  if (wasAtBottom || existingTxLines === 0) {
    requestAnimationFrame(() => { logBody.scrollTop = logBody.scrollHeight; });
  }
}

// ── Accumulated party state (merge-only, never shrink) ──
let _partyState = {};

// ── State rendering ──
function renderState(state) {
  if (!state) return;
  // Update combat badge
  const badge = document.getElementById('combat-badge');
  if (badge) {
    if (state.combat_active) {
      badge.classList.remove('hidden');
    } else {
      badge.classList.add('hidden');
    }
  }
  // Merge incoming characters into accumulated party state
  if (state.characters && Object.keys(state.characters).length > 0) {
    for (const [slug, char] of Object.entries(state.characters)) {
      if (!_partyState[slug]) {
        _partyState[slug] = { ...char };
      } else {
        // Overlay non-null values onto existing entry
        for (const [k, v] of Object.entries(char)) {
          if (v !== null && v !== undefined) _partyState[slug][k] = v;
        }
      }
    }
    renderPartyCards(_partyState);
  }
}

// ── WebSocket ──
let ws;
let wsConnected = false;
let wsInitReceived = false;
let _sessionMode = 'idle'; // 'idle' | 'live' | 'reviewing'

function setSessionMode(mode) {
  _sessionMode = mode;
  const startOverlay = document.getElementById('start-overlay');
  const reviewBanner = document.getElementById('review-banner');
  if (startOverlay) startOverlay.classList.toggle('open', mode === 'idle');
  if (reviewBanner) reviewBanner.classList.toggle('hidden', mode !== 'reviewing');
}

function startSession() {
  setSessionMode('live');
  startRecording().catch(err => console.warn('Mic denied:', err));
}

async function loadHistoryIntoStage(ts) {
  if (isRecording) stopRecording();
  closeHistory();
  const data = (_historyCurrentTs === ts && _historyCurrentData)
    ? _historyCurrentData
    : await fetch(`/api/sessions/${ts}`).then(r => r.json()).catch(() => null);
  if (!data) { alert('Failed to load session.'); return; }

  resetLogState();
  if (data.scene)      setPanel('scene', data.scene);
  if (data.map)        setPanel('map', data.map);
  if (data.next_steps) setPanel('next-steps', data.next_steps);
  if (data.story_log)  syncStoryBeats(data.story_log);
  if (data.transcript) renderTranscript(data.transcript);
  if (data.state)      renderState(data.state);

  const name = data.state?.session_name || ts;
  document.getElementById('review-session-info').textContent = `${name} · ${ts}`;
  setSessionMode('reviewing');
  requestAnimationFrame(() => { const lb = document.getElementById('log-body'); if (lb) lb.scrollTop = lb.scrollHeight; });
}

function exitReview() {
  resetLogState();
  setSessionMode('idle');
  if (ws && ws.readyState === WebSocket.OPEN) ws.close();
  else connectWS();
}

function resetLogState() {
  const logBody = document.getElementById('log-body');
  if (logBody) logBody.innerHTML = '';
  _lastStoryBeatCount = 0;
  _lastTranscriptLineCount = 0;
  _lastSceneText = '';
  _partyState = {};
}

function loadFromInit(msg) {
  // Full reset then repopulate — clean slate every reconnect
  resetLogState();

  // Panels first (scene/map/next inject into log, so render before transcript)
  for (const [name, content] of Object.entries(msg.panels || {})) {
    if (name === 'story-log') syncStoryBeats(content);
    else setPanel(name, content);
  }

  // State drives party cards with richest data
  if (msg.state && Object.keys(msg.state).length > 0) {
    renderState(msg.state);
  }

  // Transcript last — appends after beats/scene injections
  if (msg.transcript) renderTranscript(msg.transcript);

  // Scroll log to bottom after full load
  requestAnimationFrame(() => {
    const lb = document.getElementById('log-body');
    if (lb) lb.scrollTop = lb.scrollHeight;
  });

  wsInitReceived = true;
  const _hasSession = _lastTranscriptLineCount > 0;
  setSessionMode(_hasSession ? 'live' : 'idle');
  if (_hasSession) startRecording().catch(err => console.warn('Mic denied on resume:', err));
}

function connectWS() {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  ws = new WebSocket(`${proto}://${location.host}/ws`);

  ws.onopen = () => {
    wsConnected = true;
    document.getElementById('session-name').style.opacity = '1';
  };

  ws.onmessage = (e) => {
    if (_sessionMode === 'reviewing') return;
    const msg = JSON.parse(e.data);
    if (msg.type === 'init') {
      loadFromInit(msg);
    } else if (msg.type === 'panels') {
      for (const [name, content] of Object.entries(msg.data || msg.panels || {})) {
        if (name === 'story-log') syncStoryBeats(content);
        else setPanel(name, content);
      }
    } else if (msg.type === 'transcript') {
      renderTranscript(msg.content || '');
    } else if (msg.type === 'state') {
      renderState(msg.data);
    } else if (msg.type === 'decision') {
      showDecision(msg.data);
    }
  };

  ws.onclose = () => {
    wsConnected = false;
    wsInitReceived = false;
    document.getElementById('session-name').style.opacity = '0.5';
    setTimeout(connectWS, 2000);
  };
}

// ── Voice recording ──
let mediaRecorder = null;
let audioChunks = [];
let isRecording = false;
let sessionStart = null;
let timerInterval = null;

async function startRecording() {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const mimeType = MediaRecorder.isTypeSupported('audio/webm;codecs=opus')
    ? 'audio/webm;codecs=opus'
    : 'audio/webm';

  mediaRecorder = new MediaRecorder(stream, { mimeType });

  mediaRecorder.ondataavailable = (e) => {
    if (e.data.size > 0) audioChunks.push(e.data);
  };

  mediaRecorder.start(8000);

  mediaRecorder.onstop = async () => {
    if (audioChunks.length === 0) return;
    const blob = new Blob(audioChunks, { type: mimeType });
    audioChunks = [];
    await sendAudio(blob, mimeType);
  };

  window._recInterval = setInterval(() => {
    if (mediaRecorder && mediaRecorder.state === 'recording') {
      mediaRecorder.stop();
      mediaRecorder.start(8000);
    }
  }, 8000);

  isRecording = true;
  sessionStart = sessionStart || Date.now();
  updateRecUI(true);
  startTimer();
}

function stopRecording() {
  clearInterval(window._recInterval);
  if (mediaRecorder && mediaRecorder.state !== 'inactive') {
    mediaRecorder.stop();
  }
  isRecording = false;
  updateRecUI(false);
}

function toggleRecording() {
  if (isRecording) stopRecording();
  else startRecording().catch(err => alert('Microphone access denied: ' + err.message));
}

async function sendAudio(blob, mimeType, retries = 3) {
  const form = new FormData();
  form.append('audio', blob, 'audio.webm');
  for (let i = 0; i < retries; i++) {
    try {
      const resp = await fetch('/api/voice', { method: 'POST', body: form });
      const data = await resp.json();
      // Transcript arrives via WebSocket — no local append needed
      return;
    } catch (e) {
      if (i < retries - 1) await new Promise(r => setTimeout(r, 1500));
      else console.warn('STT failed after retries', e);
    }
  }
}

function updateRecUI(recording) {
  const el = document.getElementById('rec-indicator');
  const label = document.getElementById('rec-label');
  if (recording) {
    el.classList.add('recording');
    label.textContent = 'RECORDING';
  } else {
    el.classList.remove('recording');
    label.textContent = 'REC OFF';
  }
}

function startTimer() {
  if (timerInterval) return;
  timerInterval = setInterval(() => {
    if (!sessionStart) return;
    const secs = Math.floor((Date.now() - sessionStart) / 1000);
    const h = Math.floor(secs / 3600);
    const m = String(Math.floor((secs % 3600) / 60)).padStart(2, '0');
    const s = String(secs % 60).padStart(2, '0');
    document.getElementById('timer').textContent = `${h}:${m}:${s}`;
  }, 1000);
}

// ── Manual update button ──
async function forceUpdate() {
  const btn = document.getElementById('btn-update');
  btn.textContent = 'Updating…';
  btn.disabled = true;
  try {
    await fetch('/api/update', { method: 'POST' });
    setTimeout(() => { btn.textContent = 'Update'; btn.disabled = false; }, 3000);
  } catch(e) {
    btn.textContent = 'Update'; btn.disabled = false;
  }
}

// ── Panel detail (fullscreen) ──
function openPanelDetail(labelText, bodyEl) {
  document.getElementById('panel-detail-title').textContent = labelText;
  document.getElementById('panel-detail-body').innerHTML = bodyEl.innerHTML;
  document.getElementById('panel-detail-overlay').classList.add('open');
}
function closePanelDetail() {
  document.getElementById('panel-detail-overlay').classList.remove('open');
}

// ── End session modal ──
function openEndModal() {
  const check = document.getElementById('end-discard-check');
  check.checked = false;
  document.getElementById('btn-end-confirm').textContent = 'End & Archive';
  document.getElementById('btn-end-confirm').className = 'btn-modal-save';
  document.getElementById('end-session-overlay').classList.add('open');
}
function closeEndModal() {
  document.getElementById('end-session-overlay').classList.remove('open');
}
async function confirmEndSession() {
  const discard = document.getElementById('end-discard-check').checked;
  closeEndModal();
  stopRecording();
  const resp = await fetch('/api/session/end', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ discard }),
  });
  const data = await resp.json();
  if (!discard && data.release_url) alert(`Session archived.\n\nGitHub release:\n${data.release_url}`);
  sessionStart = null;
  document.getElementById('timer').textContent = '0:00:00';
  resetLogState();
  setSessionMode('idle');
}

// ── Character modal ──
let editingSlug = null;

function openAddChar() {
  editingSlug = null;
  document.getElementById('modal-title').textContent = 'Add Character';
  document.getElementById('char-name').value = '';
  document.getElementById('char-class').value = '';
  document.getElementById('char-hp-cur').value = '';
  document.getElementById('char-hp-max').value = '';
  document.getElementById('char-ac').value = '';
  document.getElementById('char-notes').value = '';
  document.getElementById('modal-overlay').classList.add('open');
  document.getElementById('char-name').focus();
}

function closeModal() {
  document.getElementById('modal-overlay').classList.remove('open');
}

// ── Decision helper ──
function showDecision(data) {
  if (!data || !data.options || data.options.length === 0) return;
  document.getElementById('decision-title').textContent = data.title || 'What do you do?';
  document.getElementById('decision-context').textContent = data.context || '';

  const optionsEl = document.getElementById('decision-options');
  optionsEl.innerHTML = '';
  for (const opt of data.options) {
    const div = document.createElement('div');
    div.className = 'decision-option';
    div.innerHTML = `
      <div class="decision-option-name">${escHtml(opt.name || '')}</div>
      ${opt.desc ? `<div class="decision-option-desc">${escHtml(opt.desc)}</div>` : ''}
      ${opt.detail ? `<div class="decision-option-detail">${escHtml(opt.detail)}</div>` : ''}
    `;
    optionsEl.appendChild(div);
  }
  document.getElementById('decision-overlay').classList.add('open');
}

function closeDecision() {
  document.getElementById('decision-overlay').classList.remove('open');
}

// ── Session history ──
let _historyCurrentTs = null;
let _historyCurrentData = null;
let _historyCurrentTab = 'scene';

function openHistory() {
  document.getElementById('history-overlay').classList.add('open');
  loadHistoryList();
}

function closeHistory() {
  document.getElementById('history-overlay').classList.remove('open');
}

async function loadHistoryList() {
  const list = document.getElementById('history-list');
  list.innerHTML = '<div style="padding:12px;color:var(--text3);font-size:12px">Loading…</div>';
  try {
    const sessions = await fetch('/api/sessions').then(r => r.json());
    list.innerHTML = '';
    if (!sessions.length) {
      list.innerHTML = '<div style="padding:12px;color:var(--text3);font-size:12px">No archived sessions yet.</div>';
      return;
    }
    for (const s of sessions) {
      const el = document.createElement('div');
      el.className = 'history-session-item';
      el.dataset.ts = s.ts;
      const name = s.session_name || s.ts;
      const loc = s.location ? ` · ${s.location}` : '';
      el.innerHTML = `
        <div class="history-session-ts">${s.ts}${loc}</div>
        <div class="history-session-name">${escHtml(name)}</div>
        ${s.scene_headline ? `<div class="history-session-scene">${escHtml(s.scene_headline)}</div>` : ''}
      `;
      el.addEventListener('click', () => loadHistorySession(s.ts, el));
      list.appendChild(el);
    }
  } catch (e) {
    list.innerHTML = '<div style="padding:12px;color:var(--red);font-size:12px">Failed to load sessions.</div>';
  }
}

async function loadHistorySession(ts, itemEl) {
  // Mark active in list
  document.querySelectorAll('.history-session-item').forEach(el => el.classList.remove('active'));
  itemEl.classList.add('active');
  _historyCurrentTs = ts;
  const detail = document.getElementById('history-detail-content');
  const empty = document.getElementById('history-detail-empty');
  detail.classList.remove('hidden');
  empty.style.display = 'none';
  document.getElementById('history-tab-body').innerHTML = '<div style="color:var(--text3);font-size:12px">Loading…</div>';
  try {
    _historyCurrentData = await fetch(`/api/sessions/${ts}`).then(r => r.json());
    document.getElementById('history-detail-ts').textContent = ts;
    document.getElementById('history-detail-name').textContent = _historyCurrentData.state?.session_name || ts;
    const recLink = document.getElementById('history-recording-link');
    if (_historyCurrentData.has_recording) {
      recLink.href = `/api/recording/${ts}`;
      recLink.classList.remove('hidden');
    } else {
      recLink.classList.add('hidden');
    }
    document.getElementById('btn-load-into-stage').classList.remove('hidden');
    renderHistoryTab(_historyCurrentTab);
  } catch (e) {
    document.getElementById('history-tab-body').innerHTML = '<div style="color:var(--red)">Failed to load session.</div>';
  }
}

function renderHistoryTab(tab) {
  _historyCurrentTab = tab;
  document.querySelectorAll('.history-tab').forEach(t => t.classList.toggle('active', t.dataset.tab === tab));
  const body = document.getElementById('history-tab-body');
  if (!_historyCurrentData) return;
  if (tab === 'scene') {
    body.innerHTML = renderScene(_historyCurrentData.scene || '');
    if (_historyCurrentData.next_steps) {
      body.innerHTML += '<hr style="margin:12px 0;border-color:var(--sep)">' + renderMd(_historyCurrentData.next_steps);
    }
  } else if (tab === 'log') {
    body.innerHTML = renderMd(_historyCurrentData.story_log || '');
  } else if (tab === 'transcript') {
    const lines = (_historyCurrentData.transcript || '').split('\n').filter(l => l.trim());
    body.innerHTML = lines.map(l => {
      const m = l.match(/^\*\*\[(\d+:\d+:\d+)\]\*\*\s*(.+)/);
      if (m) return `<div class="log-line"><span class="log-ts">${escHtml(m[1])}</span>${escHtml(m[2])}</div>`;
      return '';
    }).filter(Boolean).join('');
  } else if (tab === 'next') {
    body.innerHTML = renderMd(_historyCurrentData.next_steps || '');
  }
}

async function saveCharacter() {
  const name = document.getElementById('char-name').value.trim();
  if (!name) { alert('Name required'); return; }

  const payload = {
    name,
    char_class: document.getElementById('char-class').value.trim(),
    hp_current: parseInt(document.getElementById('char-hp-cur').value) || 0,
    hp_max: parseInt(document.getElementById('char-hp-max').value) || 0,
    ac: parseInt(document.getElementById('char-ac').value) || 0,
    notes: document.getElementById('char-notes').value.trim(),
  };

  const url = editingSlug ? `/api/characters/${editingSlug}` : '/api/characters';
  const method = editingSlug ? 'PATCH' : 'POST';

  const resp = await fetch(url, {
    method,
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
  const data = await resp.json();
  if (data.ok) {
    closeModal();
    // Refresh party cards from API
    fetch('/api/characters').then(r => r.json()).then(chars => renderPartyCards(chars));
  }
}

// ── Init ──
document.addEventListener('DOMContentLoaded', async () => {
  // Show connecting state in log
  const logBody = document.getElementById('log-body');
  if (logBody) logBody.innerHTML = '<div style="color:var(--text3);font-size:12px;padding:12px 8px;text-align:center">Connecting…</div>';

  connectWS();

  // HTTP fallback: if WS init hasn't fired within 1.5s, load via HTTP
  setTimeout(async () => {
    if (wsInitReceived) return;
    if (logBody) logBody.innerHTML = '';
    const [panels, txData, state] = await Promise.all([
      fetch('/api/panels').then(r => r.json()).catch(() => ({})),
      fetch('/api/transcript').then(r => r.json()).catch(() => ({})),
      fetch('/api/state').then(r => r.json()).catch(() => ({})),
    ]);
    for (const [name, content] of Object.entries(panels)) {
      if (name === 'story-log') syncStoryBeats(content);
      else setPanel(name, content);
    }
    if (state && Object.keys(state).length > 0) renderState(state);
    if (txData.content) renderTranscript(txData.content);
    requestAnimationFrame(() => { if (logBody) logBody.scrollTop = logBody.scrollHeight; });
  }, 1500);

  // Character select overlay — show if no character chosen yet
  if (!currentCharacter) {
    showCharSelectOverlay();
  }

  // Button bindings
  document.getElementById('rec-indicator').addEventListener('click', toggleRecording);
  document.getElementById('btn-update').addEventListener('click', forceUpdate);
  document.getElementById('btn-end-session').addEventListener('click', openEndModal);
  document.getElementById('btn-end-cancel').addEventListener('click', closeEndModal);
  document.getElementById('btn-end-confirm').addEventListener('click', confirmEndSession);
  document.getElementById('end-session-overlay').addEventListener('click', e => { if (e.target === e.currentTarget) closeEndModal(); });
  document.getElementById('end-discard-check').addEventListener('change', e => {
    const btn = document.getElementById('btn-end-confirm');
    if (e.target.checked) {
      btn.textContent = 'Discard Everything';
      btn.className = 'btn-modal-save btn-modal-danger';
    } else {
      btn.textContent = 'End & Archive';
      btn.className = 'btn-modal-save';
    }
  });
  document.getElementById('panel-scene-header').addEventListener('click', () =>
    openPanelDetail('Scene', document.getElementById('panel-scene-body')));
  document.getElementById('panel-next-header').addEventListener('click', () =>
    openPanelDetail('Next Steps', document.getElementById('panel-next-body')));
  document.getElementById('panel-detail-close').addEventListener('click', closePanelDetail);
  document.getElementById('panel-detail-overlay').addEventListener('click', e => { if (e.target === e.currentTarget) closePanelDetail(); });
  document.getElementById('btn-add-char').addEventListener('click', openAddChar);
  document.getElementById('btn-save-char').addEventListener('click', saveCharacter);
  document.getElementById('btn-cancel-char').addEventListener('click', closeModal);
  document.getElementById('decision-close').addEventListener('click', closeDecision);
  document.getElementById('btn-start-session').addEventListener('click', startSession);
  document.getElementById('btn-start-load-history').addEventListener('click', () => {
    document.getElementById('start-overlay').classList.remove('open');
    openHistory();
  });
  document.getElementById('btn-load-into-stage').addEventListener('click', () => {
    if (_historyCurrentTs) loadHistoryIntoStage(_historyCurrentTs);
  });
  document.getElementById('btn-exit-review').addEventListener('click', exitReview);
  document.getElementById('btn-history').addEventListener('click', openHistory);
  document.getElementById('history-close').addEventListener('click', closeHistory);
  document.getElementById('history-overlay').addEventListener('click', e => { if (e.target === e.currentTarget) closeHistory(); });
  document.querySelectorAll('.history-tab').forEach(t => t.addEventListener('click', () => renderHistoryTab(t.dataset.tab)));
  document.getElementById('btn-observe').addEventListener('click', () => {
    currentCharacter = null;
    localStorage.removeItem('dnd-stage-character');
    hideCharSelectOverlay();
  });

  // Close modal on overlay click
  document.getElementById('modal-overlay').addEventListener('click', (e) => {
    if (e.target === document.getElementById('modal-overlay')) closeModal();
  });

  // Close char-select overlay on bg click
  document.getElementById('char-select-overlay').addEventListener('click', (e) => {
    if (e.target === document.getElementById('char-select-overlay')) hideCharSelectOverlay();
  });

  // Map modal wiring
  document.getElementById('panel-map-header').addEventListener('click', openMapModal);
  document.getElementById('map-modal-close').addEventListener('click', closeMapModal);
  document.getElementById('map-modal-overlay').addEventListener('click', (e) => {
    if (e.target === document.getElementById('map-modal-overlay')) closeMapModal();
  });
  document.querySelectorAll('.map-tab').forEach(btn => {
    btn.addEventListener('click', () => switchMapTab(btn.dataset.tab));
  });
  document.getElementById('ddb-load-btn').addEventListener('click', loadDdbMap);

  // Log filter buttons
  document.querySelectorAll('.filter-btn').forEach(btn => {
    btn.addEventListener('click', () => toggleFilter(btn.dataset.cls));
  });

  // Keyboard shortcuts: U = force update, R = toggle recording, C = change character, Esc = close modals
  document.addEventListener('keydown', (e) => {
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA') return;
    if (e.key === 'Escape') { closeMapModal(); closeModal(); }
    if (e.key === 'u' || e.key === 'U') forceUpdate();
    if (e.key === 'r' || e.key === 'R') toggleRecording();
    if (e.key === 'c' || e.key === 'C') showCharSelectOverlay();
  });
});
