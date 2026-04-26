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

  // ─── mic capture (the iPad as the table mic) ───
  // Streams 5-second chunks to /api/voice. The server pipes each chunk through
  // STT and appends the result to the live transcript everyone sees.
  //
  // iOS / iPad Safari quirks worth knowing:
  //   1. getUserMedia REQUIRES a secure context — HTTPS or localhost. Plain
  //      HTTP over LAN throws NotAllowedError. We surface that explicitly so
  //      the user knows it's a setup thing, not a "broken" thing.
  //   2. Default MediaRecorder mimeType on iOS is `audio/mp4`, on
  //      Chrome/Firefox it's `audio/webm`. Server's ffmpeg figures it out
  //      from the file's container header so we just send what we get.
  //   3. autoplay/touch-to-start: the first getUserMedia call MUST be inside
  //      a user gesture (the tap handler).

  const recBtn   = $('rec-btn');
  const recLabel = $('rec-label');
  const recSecs  = $('rec-secs');
  const recError = $('rec-error');

  let mediaStream   = null;
  let isRecording   = false;
  let recStarted    = 0;
  let recTimer      = null;
  let recMime       = '';
  let inflight      = 0;
  let chunksSeen    = 0;
  let chunksUploaded = 0;
  let chunksFailed   = 0;
  let chunksSkipped  = 0;   // VAD-skipped (silent room tone)
  // Whisper was trained on ~30s clips and accuracy drops sharply below ~10s.
  // 5s was too short — game narration like "Spock shoots the alien for 3
  // damage" was getting lost or replaced with hallucinated boilerplate. 10s
  // gives whisper enough context to anchor on, at the cost of slightly
  // more transcript latency.
  const SLICE_MS    = 10000;

  // ─── Voice activity detection ───
  // Whisper hallucinates "Thank you for watching" / "Subtitles by…" on
  // near-silent audio. We solve it at the source: keep an AnalyserNode tap
  // on the mic, sample its time-domain data ~25× per second across the
  // whole slice, track the peak normalized amplitude. If the peak never
  // crossed VAD_THRESHOLD by slice end, we drop the chunk WITHOUT uploading.
  // The audio is gone (we didn't record to disk on the server side) but
  // that's fine — there was nothing to capture. URL `?vad=off` disables.
  //
  // Threshold tuning: getByteTimeDomainData returns 0..255 with 128 = silence.
  // Subtracting 128 yields a signed sample; abs/255 normalizes to 0..1.
  // 0.02 (≈ −34 dBFS) is a good "someone in the room is talking, not just
  // breathing or HVAC" threshold. Bump to 0.04 if false positives are bad,
  // drop to 0.012 for "whispering across a quiet table". Override via
  // `?vad-threshold=0.03` for live tweaks.
  const VAD_ENABLED   = (new URLSearchParams(location.search).get('vad') !== 'off');
  const VAD_THRESHOLD = (() => {
    const q = parseFloat(new URLSearchParams(location.search).get('vad-threshold'));
    return Number.isFinite(q) && q > 0 ? q : 0.02;
  })();
  let audioCtx = null, analyser = null, vadTimer = null;
  let vadBuf = null, vadPeak = 0;

  // Mirror mic-pipeline events to the server log so we can grep for them in
  // `docker logs adventurer-live | grep input_debug` and diagnose without
  // peering at iPad screen text.
  function micLog(payload) {
    try {
      fetch('/api/debug/input', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ ...payload, source: 'player-mic' }),
        keepalive: true,
      }).catch(() => {});
    } catch {}
  }

  // Quick context check up-front. If we're plainly insecure on a non-localhost
  // host, the mic will fail no matter what; pre-warn the user.
  if (!window.isSecureContext &&
      location.hostname !== 'localhost' &&
      location.hostname !== '127.0.0.1') {
    showMicError(
      `<strong>This page is on plain HTTP.</strong> ` +
      `iPad Safari and Chrome require a secure context (HTTPS) for microphone access. ` +
      `Tap the button anyway and Safari will probably show a "no microphone" error — ` +
      `then the DM needs to either expose adventurer over HTTPS (e.g. via Cloudflare ` +
      `Tunnel) or accept a self-signed cert.`
    );
  }

  recBtn.addEventListener('click', async () => {
    if (isRecording) stopRecording();
    else await startRecording();
  });

  // Stop+restart pattern: each chunk is its own short-lived MediaRecorder
  // session that produces a COMPLETE webm/mp4 file (with proper container
  // headers). MediaRecorder.start(timeslice) is unreliable on iOS Safari and
  // some other browsers — only the first chunk gets headers; subsequent ones
  // are raw codec data and ffmpeg/whisper can't decode them ([BLANK_AUDIO]
  // for every chunk after the first). One getUserMedia call though — we
  // reuse the underlying audio track across recorder cycles so there's no
  // re-prompt and no audio gap.
  async function startRecording() {
    if (isRecording) return;
    hideMicError();
    try {
      mediaStream = await navigator.mediaDevices.getUserMedia({
        audio: {
          echoCancellation: true,
          noiseSuppression: true,
          autoGainControl: true,
        },
      });
    } catch (e) {
      console.warn('getUserMedia failed', e);
      showMicError(formatGetUserMediaError(e));
      micLog({ type: 'mic_getusermedia_fail', name: e && e.name, msg: String(e) });
      return;
    }
    recMime = pickMime();
    chunksSeen = chunksUploaded = chunksFailed = chunksSkipped = 0;
    isRecording = true;
    recStarted = Date.now();
    setRecordingUI(true);
    recTimer = setInterval(updateTimer, 250);

    // Wire up the VAD analyser tap. Same MediaStream as the recorder
    // (no extra getUserMedia call), so iOS Safari only prompts once.
    if (VAD_ENABLED) {
      try {
        const Ctor = window.AudioContext || window.webkitAudioContext;
        audioCtx = new Ctor();
        const src = audioCtx.createMediaStreamSource(mediaStream);
        analyser = audioCtx.createAnalyser();
        analyser.fftSize = 1024;          // 512 time-domain samples
        analyser.smoothingTimeConstant = 0;
        src.connect(analyser);            // NOT connected to destination — silent
        vadBuf = new Uint8Array(analyser.fftSize);
        vadPeak = 0;
        // Sample every 40ms (~25 Hz). At fftSize=1024 / sampleRate≈48000 each
        // window covers ~21ms, so 40ms polling sees every chunk of audio
        // without overlap-induced double-counts.
        vadTimer = setInterval(() => {
          if (!analyser) return;
          analyser.getByteTimeDomainData(vadBuf);
          // Peak abs deviation from 128 (silence midpoint), normalized 0..1.
          let peak = 0;
          for (let i = 0; i < vadBuf.length; i++) {
            const a = Math.abs(vadBuf[i] - 128);
            if (a > peak) peak = a;
          }
          const norm = peak / 128;
          if (norm > vadPeak) vadPeak = norm;
        }, 40);
      } catch (e) {
        micLog({ type: 'mic_vad_init_fail', err: String(e) });
        analyser = null;  // disable for this session, recorder still runs
      }
    }

    micLog({
      type: 'mic_started',
      pattern: 'stop_restart',
      mime: recMime,
      slice_ms: SLICE_MS,
      vad: { enabled: VAD_ENABLED, threshold: VAD_THRESHOLD, active: !!analyser },
      tracks: mediaStream.getAudioTracks().map(t => ({
        label: t.label, settings: t.getSettings(),
      })),
      ua: navigator.userAgent,
    });
    recordOneSlice();
  }

  function recordOneSlice() {
    if (!isRecording || !mediaStream) return;
    let recorder;
    try {
      recorder = recMime
        ? new MediaRecorder(mediaStream, { mimeType: recMime })
        : new MediaRecorder(mediaStream);
    } catch (e) {
      micLog({ type: 'mic_recorder_init_fail', err: String(e) });
      stopRecording();
      return;
    }
    let blob = null;
    // Reset the per-slice peak BEFORE recorder.start() so this slice's
    // audio energy is what gets measured (not lingering peak from before).
    vadPeak = 0;
    recorder.ondataavailable = (e) => {
      if (e.data && e.data.size > 0) blob = e.data;
    };
    recorder.onerror = (e) => {
      micLog({ type: 'mic_recorder_error', err: String(e && e.error || e) });
    };
    recorder.onstop = async () => {
      // Snapshot the slice peak *before* starting the next slice (which would
      // reset it). VAD gate: if the peak never crossed threshold, skip the
      // upload entirely. Whisper would just hallucinate on it.
      const slicePeak = vadPeak;
      if (blob) {
        if (analyser && slicePeak < VAD_THRESHOLD) {
          chunksSkipped++;
          micLog({
            type: 'mic_chunk_skipped_vad',
            n: chunksSeen + 1,
            bytes: blob.size,
            peak: +slicePeak.toFixed(4),
            threshold: VAD_THRESHOLD,
          });
        } else {
          await onChunk({ data: blob, peak: slicePeak });
        }
      }
      if (isRecording) recordOneSlice();   // immediately start next slice
    };
    recorder.start();
    // Stop after SLICE_MS — guarded against the recorder dying early.
    setTimeout(() => {
      try {
        if (recorder.state === 'recording') recorder.stop();
      } catch (e) {
        micLog({ type: 'mic_stop_throw', err: String(e) });
      }
    }, SLICE_MS);
  }

  function stopRecording() {
    if (!isRecording) return;
    isRecording = false;
    micLog({ type: 'mic_stopped',
             chunks_seen: chunksSeen, uploaded: chunksUploaded,
             failed: chunksFailed, vad_skipped: chunksSkipped });
    if (mediaStream) {
      mediaStream.getTracks().forEach(t => t.stop());
      mediaStream = null;
    }
    // Tear down the VAD analyser. Need to close() the AudioContext or
    // iOS keeps the mic-permission indicator on after the recorder stops.
    if (vadTimer) { clearInterval(vadTimer); vadTimer = null; }
    if (audioCtx) {
      try { audioCtx.close(); } catch {}
      audioCtx = null;
    }
    analyser = null;
    vadBuf = null;
    vadPeak = 0;
    setRecordingUI(false);
    clearInterval(recTimer); recTimer = null;
    recSecs.textContent = '';
  }

  function setRecordingUI(rec) {
    recBtn.classList.toggle('recording', rec);
    recLabel.textContent = rec ? 'Recording — tap to stop' : 'Tap to talk';
  }

  function updateTimer() {
    const s = Math.floor((Date.now() - recStarted) / 1000);
    const mm = Math.floor(s / 60).toString();
    const ss = (s % 60).toString().padStart(2, '0');
    // Live VAD pip — green dot if voice currently above threshold (this
    // 250ms tick), grey dot otherwise. Lets the player verify their mic
    // is actually picking them up during the game.
    let pip = '';
    if (analyser) {
      pip = vadPeak >= VAD_THRESHOLD ? ' 🟢' : ' ⚪';
    }
    let skipped = chunksSkipped > 0 ? ` (skipped ${chunksSkipped})` : '';
    recSecs.textContent = `${mm}:${ss}${pip}` + (inflight > 0 ? '  ⤴' : '') + skipped;
  }

  function pickMime() {
    const candidates = [
      'audio/webm;codecs=opus',
      'audio/webm',
      'audio/ogg;codecs=opus',
      'audio/mp4',                  // Safari / iOS default
      'audio/mp4;codecs=mp4a.40.2',
    ];
    for (const m of candidates) {
      try { if (MediaRecorder.isTypeSupported && MediaRecorder.isTypeSupported(m)) return m; }
      catch {}
    }
    return '';
  }

  async function onChunk(evt) {
    chunksSeen++;
    micLog({ type: 'mic_chunk', n: chunksSeen, bytes: evt.data ? evt.data.size : 0,
             rec_state: mediaRecorder ? mediaRecorder.state : 'gone' });
    if (!evt.data || !evt.data.size) return;
    const ext = (recMime && recMime.includes('mp4')) ? 'm4a'
              : (recMime && recMime.includes('ogg')) ? 'ogg' : 'webm';
    const fd = new FormData();
    fd.append('audio', evt.data, `chunk-${Date.now()}.${ext}`);
    inflight++;
    recBtn.classList.add('uploading');
    try {
      const resp = await fetch('/api/voice', { method: 'POST', body: fd });
      if (resp.ok) {
        chunksUploaded++;
        micLog({ type: 'mic_upload_ok', n: chunksSeen });
      } else {
        chunksFailed++;
        const txt = await resp.text();
        console.warn('chunk upload failed', resp.status, txt);
        micLog({ type: 'mic_upload_fail', n: chunksSeen, status: resp.status, body: txt.slice(0, 200) });
      }
    } catch (e) {
      chunksFailed++;
      console.warn('chunk upload network error', e);
      micLog({ type: 'mic_upload_neterr', n: chunksSeen, err: String(e) });
    } finally {
      inflight--;
      if (inflight === 0) recBtn.classList.remove('uploading');
    }
  }

  function showMicError(html) {
    recError.classList.remove('hidden');
    recError.innerHTML = html;
  }
  function hideMicError() {
    recError.classList.add('hidden');
    recError.innerHTML = '';
  }

  function formatGetUserMediaError(e) {
    const name = e && e.name || '';
    const msg = e && (e.message || String(e)) || 'unknown';
    if (name === 'NotAllowedError' || name === 'SecurityError') {
      return `<strong>Microphone access blocked.</strong><br>` +
             `(${name}: ${msg})<br><br>` +
             `Most likely cause: this page is on plain HTTP. iPad Safari and ` +
             `Chrome require HTTPS for the mic. Ask the DM to expose adventurer ` +
             `over HTTPS, or visit via <code>localhost</code> from the host machine.`;
    }
    if (name === 'NotFoundError') {
      return `<strong>No microphone detected on this device.</strong> ` +
             `(${msg})`;
    }
    return `<strong>Could not start mic.</strong><br>(${name}: ${msg})`;
  }

  // Stop recording when the player tab goes away (background, page close).
  window.addEventListener('beforeunload', stopRecording);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden') stopRecording();
  });
})();
