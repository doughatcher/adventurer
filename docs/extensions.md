# Extensibility model — design proposal

**Status:** Design doc, not yet implemented. Captures the architectural
direction so future code lands in the right shape.

## What's core, what's not

The user-articulated split — and the right one:

| Layer        | Responsibility                                                       | Lives in            |
| ------------ | -------------------------------------------------------------------- | ------------------- |
| **Core**     | Capture transcript → parse with LLM → update characters/scene/intent | binary, always on   |
| **Core**     | Player identity (characters as logins, persistent auth)              | binary, always on   |
| **Core**     | Session lifecycle (start/load/save), session archive                 | binary, always on   |
| **Core**     | DM stage UI shell + WebSocket fanout                                 | binary, always on   |
| **Plugin**   | D&D Beyond character sheet sync                                      | optional extension  |
| **Plugin**   | Map / battlemap renderer                                             | optional extension  |
| **Plugin**   | Initiative tracker                                                   | optional extension  |
| **Plugin**   | Sound effects / music cue trigger                                    | optional extension  |
| **Plugin**   | Dice-roll detection + visualization                                  | optional extension  |
| **Plugin**   | Voice TTS for the DM's narration                                     | optional extension  |
| **Plugin**   | Critrole-style auto-recap to a wiki                                  | optional extension  |

Anything that *interprets the transcript* and writes back into the structured
state is a candidate for core (it's why the product exists). Anything that
*displays differently* or *integrates with another service* is a plugin.

## Plugin contract (proposed)

A plugin is a **stand-alone process** the server discovers at startup, talks
to over a small JSON-RPC-ish protocol on stdin/stdout, and surfaces in the UI
via a manifest. Same shape as the existing LLM/STT worker IPC — we already
know it works, and it sidesteps every dynamic-loading/ABI nightmare.

### Discovery

```
~/.local/share/adventurer/plugins/
├── dnd-beyond/
│   ├── plugin.toml          ← manifest
│   ├── plugin               ← native exe (Linux)
│   ├── plugin.exe           ← native exe (Windows)
│   └── ui/                  ← optional client-side assets
│       ├── panel.html       ← injected into a slot the manifest names
│       ├── panel.js
│       └── panel.css
└── map/
    ├── plugin.toml
    └── plugin
```

The server scans this dir on start (and on a `dev_reload`-style watcher in
dev mode). Plugins ship as a separate Steam content depot or as a Workshop
download — the binary itself stays small.

### `plugin.toml`

```toml
[plugin]
id          = "dnd-beyond"
name        = "D&D Beyond character sheet sync"
version     = "0.1.0"
author      = "Doug Hatcher"
description = "Pulls character data from beyond.dndbeyond.com on demand."

# What event topics the plugin wants to subscribe to.
subscribes = ["state.updated", "transcript.appended", "player.assigned"]

# What HTTP routes the plugin wants the server to mount on its behalf.
# Routes get prefixed with /ext/{id}/… so collisions are impossible.
[[routes]]
method = "POST"
path   = "import"            # → POST /ext/dnd-beyond/import

[[routes]]
method = "GET"
path   = "character/{slug}"  # → GET  /ext/dnd-beyond/character/:slug

# What slots the plugin's UI wants to render into.
[ui]
panels = ["character-sheet"]   # adds a tab in the DM stage panel area
player_widgets = ["sheet-link"]  # adds a button on the player view header
inject_script  = "ui/panel.js"   # gets <script>-injected like dev-reload.js
inject_style   = "ui/panel.css"

# State shape extensions the plugin owns (namespaced to avoid LLM clobber).
[state_schema]
# Characters get a per-character namespace; the LLM-update path explicitly
# ignores any key under `ext.*` so plugins can stash data here without
# fighting with state extraction.
character_ext = "dndbeyond"     # → state.characters[*].ext.dndbeyond = { … }
```

### Wire protocol

Same line-delimited JSON the LLM/STT workers already speak:

```jsonc
// Server → plugin (events the plugin subscribed to)
{ "type": "event", "topic": "state.updated", "payload": { … } }
{ "type": "event", "topic": "transcript.appended", "payload": { "text": "…" } }

// Plugin → server (push state mutation)
{ "type": "patch", "path": "/characters/Aryn/ext/dndbeyond", "value": { "hp": 24 } }

// Plugin → server (broadcast custom WS event)
{ "type": "broadcast", "event": { "type": "ext.dndbeyond.synced", "slug": "Aryn" } }

// Server → plugin (HTTP request the server is proxying through)
{ "type": "http_request", "id": 17, "method": "POST", "path": "/import", "body": "…" }

// Plugin → server (HTTP response)
{ "type": "http_response", "id": 17, "status": 200, "body": { "ok": true } }
```

Patches use RFC 6902 JSON Patch shape so multiple plugins can mutate
disjoint subtrees without locking the whole state. The server applies
them, persists state.json, and broadcasts `state` over WS as today.

### Why subprocess + JSON-RPC instead of dylib / Wasm?

- **Subprocess matches what we already do.** LLM and STT workers already
  talk this protocol. Zero new infrastructure.
- **Crash isolation.** A plugin SEGV doesn't take down the server. The
  watcher restarts it.
- **Language-agnostic.** Plugins can be Python, Node, Rust, Go — anyone
  can write one.
- **Sandboxable later.** When we ship through Steam we can run plugin
  procs under a `seccomp` / Job Object profile. With dylib we'd inherit
  full process privileges.
- **Wasm is tempting but not yet.** WIT/WASI Preview 2 isn't quite there
  for our use cases (filesystem + sockets + GPU access for a TTS plugin).
  Subprocess buys us the same isolation today with less ecosystem risk.

### Failure modes covered

- Plugin doesn't start → log the manifest + crash, mark it `disabled`,
  surface in DM panel "extensions" tab.
- Plugin hangs → 5 s timeout per RPC, drop the call, no event delivery
  blocks the server.
- Plugin floods state patches → debounce with the same 250 ms window the
  LLM update loop uses.
- Plugin's UI throws → `<script>` is injected with `defer` and an
  error-boundary in the host HTML so it doesn't kill the DM stage JS.

## Persistent character auth (concrete next step)

Even before the full plugin model, the user wants this to "just work":

> *"Characters as logins I think is great and should be core. I think
> some sort of auth parameter in the qr code and in the invites could
> prevent security issues. Maybe you even maintain a list of auths so
> like I don't have to worry about re-authing when I restart the app —
> my tab will just keep working like it has been."*

Current state of player auth:

- Player tab generates a random 16-byte token in `localStorage` on first
  visit (`adv-player-token`), POSTs `/api/players/announce`, reuses it
  forever on that device.
- Server-side `Players` map is **in-memory only** — server restart
  forgets every token. The tab still has its token and re-announces, but
  the **character assignment** the DM made is lost (player has to be
  re-assigned to "Aryn the Druid" every restart).
- The QR encodes only the join URL, no per-player auth. Anyone on the
  same network who scans it can join. Fine for in-person home games,
  not OK for "share over Discord".

### Proposed changes

**1. Persist the player roster to disk.** Just like `state.json` and
`panels/`, mirror `Players` to `${SESSION_DIR}/players.json` on every
mutation, restore on startup. Tokens in localStorage stay valid across
server restarts.

```jsonc
// ${SESSION_DIR}/players.json
{
  "version": 1,
  "players": [
    {
      "token": "P-7f3c…",        // opaque, random, 16 bytes b64
      "label": "Tom's iPad",     // friendly hint
      "character": "Aryn",        // slug into state.characters
      "first_seen": "2026-04-26T15:00:00Z",
      "last_seen":  "2026-04-26T22:30:00Z",
      "scope": "player"           // future: "player" | "spectator" | "co-dm"
    }
  ]
}
```

**2. Per-session invite codes (DM-issued).** New endpoint:

```
POST /api/players/invite          → { "invite_code": "ADV-7H3K-9X2P", "url": "…?invite=ADV-7H3K-9X2P" }
GET  /api/players/invites         → list active invites
DELETE /api/players/invites/:code → revoke
```

The QR can encode `…/join?invite=ADV-7H3K-9X2P`. On scan, the player UI
exchanges the invite for a long-lived player token via:

```
POST /api/players/redeem  { "invite": "ADV-7H3K-9X2P", "label": "Tom's iPad" }
  → { "token": "P-7f3c…", "expires_at": null }
```

Invites are **single-use by default** (delete on redeem) but can be
flagged `multi_use=true` for the "open invite for tonight" case. If
expired or already-redeemed, the redeem POST returns 401 and the player
view shows a friendly "ask the DM for a new code".

**3. Character bind.** Once redeemed, the DM clicks the player in the
roster and picks a character → `/api/players/{token}/assign`. The
character slug is now sticky on that token across server restarts (because
players.json is persisted).

**4. Token format + threat model.**
- `P-` prefix + 22 chars of base64url(16 random bytes) ≈ 128 bits entropy.
- Tokens are bearer secrets — anyone holding one *is* that player. So:
  - Always on HTTPS in production (already true via Cloudflare Tunnel).
  - Player tab logs out of localStorage if the server says 401 (revoked).
  - Token never goes in URL after redeem — only in `Authorization:
    Bearer …` header / WS subprotocol header.
  - Future: WebAuthn passkey for "DM identity on a new device". Not
    needed for v1.

**5. The QR contains an invite, not a token.** Scanning a QR with my
phone, walking out of the house, and someone on the LAN later loading the
QR image off my phone → can't reuse the QR to join (single-use redeemed
already). DM hits the "🔄 New invite" button to rotate.

**6. WebSocket auth.** Today: `?role=player&token=…`. Add server-side
validation against the persisted roster — unknown tokens get a
`auth_required` event back over WS, player UI prompts for re-redeem.

**7. Audit trail.** Append to `${SESSION_DIR}/players-audit.jsonl`:

```jsonc
{"ts":"…","event":"invite_created","code":"ADV-7H3K-9X2P","by":"dm"}
{"ts":"…","event":"redeemed","code":"ADV-7H3K-9X2P","token":"P-7f3c…","label":"Tom's iPad"}
{"ts":"…","event":"assigned","token":"P-7f3c…","character":"Aryn"}
{"ts":"…","event":"revoked","token":"P-7f3c…","by":"dm"}
```

Same never-delete philosophy as the audio archive. Helps with "who said
what" if a player tab drops mid-session.

### Migration from the current model

- Existing player tabs send their localStorage token. If it's not in
  `players.json` (because we just rolled out persistence and they joined
  pre-rollout), accept it on first contact, write it to players.json
  with `legacy: true`, and surface a "Migrated 1 legacy player" notice
  to the DM. No re-auth needed.
- The "anyone on the LAN can join with the QR" mode stays as a launcher
  flag (`ADVENTURER_OPEN_JOIN=1`) for the home-game case where security
  is not a concern.

## Implementation order (when we come back to this)

Strict ordering — each unblocks the next:

1. **Persist `Players`** to `${SESSION_DIR}/players.json` + restore on
   startup. (Smallest, highest-value win — solves the immediate user
   pain of re-assigning characters after every restart.)
2. **Invite/redeem flow** — new endpoints + QR encodes invite + player
   tab calls redeem on first load.
3. **Audit log** for player events.
4. **Plugin contract v0**: just the manifest + the subprocess + the
   subscribe/patch JSON-RPC. No UI injection yet.
5. **Plugin UI injection** — `inject_script` / `inject_style` + a panel
   slot system in the DM stage HTML. Reuses the existing dev-reload
   injection pattern.
6. **First real plugin** as a proof: a tiny `dndbeyond` stub that just
   exposes a "Sync from D&D Beyond" button per character and stashes a
   placeholder JSON under `state.characters.*.ext.dndbeyond`.

Each step is independently shippable and reversible.

## Open questions

- **Plugin distribution.** Steam Workshop fits ("publish your D&D Beyond
  plugin to Workshop, subscribers auto-install"). Pre-Steam: a simple
  `~/.local/share/adventurer/plugins/` drop-in dir, manually populated.
- **Permissions UX.** Plugin asks for "internet access" / "filesystem
  read" / "subscribe to transcripts" — present a one-time prompt to the
  DM at install time. Same shape as browser extensions.
- **State namespace.** Settled on `state.characters[*].ext.{plugin_id}`
  for per-character data and `state.ext.{plugin_id}` for global data.
  LLM update path explicitly preserves anything under `ext.*` (we'd add
  this guard in `gemma.rs`).
- **Co-DM / spectator scopes.** Hinted at in the `scope` field above.
  Worth designing now even if not built. Spectator = read-only WS, can't
  POST audio. Co-DM = full DM rights, can edit panels, can issue
  invites. Multi-DM lets two people split DM duties (one runs combat,
  one runs roleplay).
