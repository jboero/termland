# Protocol and data structures

Reference for Termland's wire protocol: the framing, every message, the
negotiation rules, and — most importantly — the compatibility contract that
governs changing any of it.

Everything here lives in `crates/termland-protocol`. That crate is shared by
the server, the desktop client and `termland-mobile-core` (the Android client),
so it is the single definition of the wire format; there is no second
implementation to keep in step.

| File | Contents |
|---|---|
| `messages.rs` | The `Message` envelope and every control/data-plane struct |
| `frame.rs` | `TermlandCodec` — length-prefixed framing over a byte stream |
| `input.rs` | Keyboard and pointer events |
| `clipboard_files.rs` | `text/uri-list` parsing and filename sanitisation |

Related design docs: [`quic-transport.md`](quic-transport.md) for how the planes
are split over QUIC, and [`mobile-clients.md`](mobile-clients.md).

## Framing

Every message on the control plane is CBOR, length-prefixed with a 7-byte
header:

```text
┌──────────┬──────────┬────────────────────┬──────────────────┐
│ magic 2B │ msg_id 1B│ payload length 4B  │ CBOR payload     │
│  "TL"    │          │ little-endian      │                  │
└──────────┴──────────┴────────────────────┴──────────────────┘
```

- Magic is `0x54 0x4C`. A mismatch is a hard error (`CodecError::InvalidMagic`)
  rather than a resync attempt — a desynchronised stream is not recoverable and
  failing loudly beats decoding garbage.
- Payload length is capped at `MAX_PAYLOAD_SIZE` (16 MiB). Larger is rejected
  before allocating, so a malformed or hostile length cannot be used to make the
  peer reserve arbitrary memory.
- `msg_id` duplicates the variant tag inside the CBOR. It is redundant by
  design: it allows routing or logging a frame without deserialising the body.

The same framing is used over TCP, over the SSH subsystem's stdin/stdout, and
on QUIC's control stream. Only the *media* planes differ per transport — see
below.

## Message envelope

`Message` is an externally-tagged enum; `MessageId` gives each variant a stable
byte value. The numbering is grouped, and the gaps are deliberate — room to add
within a group without renumbering:

| Range | Direction | Purpose |
|---|---|---|
| `0x01`–`0x0F` | both | Control plane: handshake, auth, session lifecycle |
| `0x20`–`0x25` | server → client | Media and server-originated data |
| `0x40`–`0x48` | client → server | Input and client-originated data |

## Session lifecycle

```text
client                                        server
  │                                              │
  │──── Hello { protocol_version, name } ───────▶│
  │◀─── HelloAck { version, name, auth_required }│
  │                                              │
  │   (only when auth_required)                  │
  │◀─── AuthRequest { methods }                  │
  │──── AuthResponse { username, credential } ──▶│
  │◀─── AuthResult { success, message }          │
  │                                              │
  │──── SessionCreate {…} ──or── SessionAttach ─▶│
  │◀─── SessionReady { size, codec, session_id } │
  │                                              │
  │◀═══ VideoFrame / AudioChunk / CursorUpdate ══│
  │═══▶ KeyEvent / MouseMove / MouseButton ══════│
  │                                              │
  │◀─── SessionEnd { reason }                    │
```

`PROTOCOL_VERSION` is currently **1**. It has not been incremented, because
every change so far has been additive under the rules below — see
*Compatibility*.

### Handshake

| Message | Fields |
|---|---|
| `Hello` | `protocol_version: u32`, `client_name: String` |
| `HelloAck` | `protocol_version: u32`, `server_name: String`, `session_id: String`, `auth_required: bool` |

`auth_required` tells the client whether the server was started with `--auth`.
In SSH-subsystem mode authentication has already happened at the SSH layer, so
this is false and no auth exchange follows.

### Authentication

| Message | Fields |
|---|---|
| `AuthRequest` | `methods: Vec<String>` |
| `AuthResponse` | `username: String`, `credential: String` |
| `AuthResult` | `success: bool`, `message: String` |

Credentials are validated against PAM. This exchange is plaintext within the
stream, so it is only safe under TLS or an SSH tunnel — the server logs a loud
warning when `--auth` is used without `--tls`.

A successful authentication also determines the *system user* a newly created
session's compositor is isolated into. That identity comes from server-side
auth state only, never from anything the client sends in `SessionCreate`.

### Creating and resuming

`SessionCreate` starts a new session:

| Field | Type | Notes |
|---|---|---|
| `mode` | `SessionMode` | `Desktop`, or `App { command, args }` for kiosk mode |
| `width`, `height` | `u32` | Initial output size |
| `audio` | `bool` | Whether to capture and stream audio |
| `quality` | `u8` | 1–100, default 75; maps to encoder bitrate/CRF |
| `desktop_shell` | `Option<String>` | Startup command inside labwc; auto-detected when absent |
| `encoder_preset` | `Option<String>` | Backend-specific override |
| `encoder_crf` | `Option<u8>` | Backend-specific override |
| `encoder_extra_params` | `Option<String>` | Passed verbatim as `svtav1-params` |
| `supported_codecs` | `Vec<VideoCodec>` | Client's decodable video codecs, in preference order |
| `supported_audio_codecs` | `Vec<AudioCodec>` | Client's decodable audio codecs |

`desktop_shell` and `App.command` are **client-supplied strings that the server
executes**. They are validated by `validate_shell_command` before any shell use.
Treat that validation as security-critical.

`SessionAttach` resumes an existing session by `session_id`, carrying the same
quality/encoder/codec fields — a session can be resumed by a different client
build than created it, so capabilities are re-advertised rather than remembered.

`SessionReady` closes either path:

| Field | Type | Notes |
|---|---|---|
| `width`, `height` | `u32` | Actual size, which may differ from requested |
| `xkb_keymap` | `Option<String>` | Server's keymap, for correct scancode mapping |
| `codec` | `Option<VideoCodec>` | Negotiated video codec |
| `audio_codec` | `Option<AudioCodec>` | Negotiated audio codec; `None` means no audio |
| `session_id` | `String` | Stable id for later reattachment; empty if not persistent |

Announcing the codecs here lets the client build matching decoders up front
instead of inferring them from the first frames.

### Listing, resizing, ending

| Message | Fields |
|---|---|
| `SessionList` | *(empty)* |
| `SessionListResult` | `sessions: Vec<SessionInfo>` |
| `SessionInfo` | `session_id`, `mode`, `width`, `height`, `age_secs`, `attached` |
| `SessionClose` | `session_id: String` |
| `SessionResize` | `width: u32`, `height: u32` |
| `SessionEnd` | `reason: String` |
| `Ping` / `Pong` | `timestamp_us: u64` |

When the server runs with `--auth`, listing, attaching and closing are
restricted to the session's owner, so one authenticated user cannot enumerate
or hijack another's session by guessing an id.

## Media plane

| Message | Fields |
|---|---|
| `VideoFrame` | `timestamp_us`, `frame_type` (`Keyframe`/`Inter`), `width: u16`, `height: u16`, `codec: Option<VideoCodec>`, `data: bytes` |
| `StillFrame` | `timestamp_us`, `x`, `y`, `width`, `height`, `lossless: bool`, `data: bytes` |
| `AudioChunk` | `timestamp_us`, `sample_rate: u32`, `channels: u8`, `data: bytes` |
| `CursorUpdate` | `x`, `y`, `hotspot_x`, `hotspot_y`, `width`, `height`, `visible`, `image_rgba: bytes` |

Every payload uses a CBOR byte string rather than an array of integers — the
difference is roughly 1 byte per byte versus 2–3, which matters at video rates.

`VideoFrame.codec` tags each frame so a client can select a decoder
deterministically instead of sniffing the bitstream. `StillFrame` carries a
rectangle for partial updates, used for static content where sending full video
frames would be wasteful.

`CursorUpdate` supports client-side cursor rendering: with `CursorModeMsg
{ include_cursor_in_frame: false }` the cursor is excluded from the encoded
video and drawn locally, so pointer motion does not make a round trip through
the encoder.

### Transport differences

Over TCP and SSH every message — media included — is a CBOR frame on the single
stream. **QUIC (Q2) is different**: video moves to a dedicated uni stream with a
fixed 18-byte binary header, and audio to datagrams with a 5-byte header, so a
lost audio packet cannot stall video and neither can stall control. The control
plane is unchanged. See [`quic-transport.md`](quic-transport.md).

## Input plane

| Message | Fields |
|---|---|
| `KeyEvent` | `scancode: u32`, `keysym: u32`, `state` (`Pressed`/`Released`/`Repeat`), `modifiers: u32` |
| `MouseMove` | `x: f64`, `y: f64`, `absolute: bool` |
| `MouseButton` | `button: u32`, `state` (`Pressed`/`Released`) |
| `MouseScroll` | `dx: f64`, `dy: f64` |
| `TextInput` | `text: String` |
| `QualityHintMsg` | `max_fps: u8`, `max_bitrate_kbps: u32`, `prefer_lossless: bool` |
| `CursorModeMsg` | `include_cursor_in_frame: bool` |

`TextInput` is deliberately separate from `KeyEvent`. A `KeyEvent` carries an
evdev scancode, which only has meaning against the server's fixed xkb keymap and
therefore cannot express codepoints outside that layout — no emoji, no CJK
commit, no autocorrect result from a soft keyboard. Text arriving via
`TextInput` has no scancode and is injected by synthesising a temporary keymap
server-side.

Editing and shortcut keys (Backspace, Enter, arrows, Ctrl+C) are *not* text and
must keep using `KeyEvent`.

## Clipboard and file transfer

| Message | Fields |
|---|---|
| `ClipboardData` / `ClipboardSend` | `ClipboardPayload { mime_type: String, data: bytes }` |
| `FileTransferData` / `FileTransferSend` | `FileTransferPayload { files: Vec<FileEntry> }` |
| `FileEntry` | `name: String`, `data: bytes` |

`FileEntry.name` is **attacker-controllable data from the far end of the
connection**. It must be passed through `clipboard_files::sanitize_filename`
before touching the filesystem; that function rejects absolute paths, `..`
traversal, embedded separators, and `.`/`..`/empty names.

File transfer is capped at `MAX_FILE_TRANSFER_BYTES` (12 MiB) total across all
files, sent as a single message with no chunking — an MVP limitation, sized to
leave headroom under the 16 MiB frame cap for CBOR overhead. A list exceeding
the cap is rejected and logged by the sender, never silently truncated.

## Codec negotiation

Video and audio negotiate identically: the client advertises what it can
*decode* in preference order, the server intersects that with what it can
*encode*, and `SessionReady` reports the result.

```text
client advertises  [Av1, Vp9, Vp8, H265, H264]
server can encode  [Vp9, H264]              (whatever probing found)
                →  Vp9                       (client's order wins)
```

The client's ordering is authoritative, so a client can express a real
preference rather than accepting whatever the server likes best.

`VideoCodec`: `Av1`, `Vp9`, `Vp8`, `H265`, `H264` — listed open-source-first,
then patent-encumbered. `AudioCodec`: `Opus` only today; the enum exists so a
second codec is a variant plus an encoder backend rather than a wire change.

The two diverge when there is no overlap at all:

- **Video** fails the session — without a usable encoder there is nothing to
  send. Note the current mechanism is a `panic!` in the capture thread rather
  than a clean `SessionEnd { reason }`, so the client sees the connection drop
  instead of an explanation. Worth improving.
- **Audio** runs the session **silently** and logs why. Streaming audio a client
  cannot decode would look like working audio that produces no sound, which is
  far harder to diagnose than no audio at all.

## Compatibility contract

**Read this before changing anything in `messages.rs`.**

Deployed clients and servers are not upgraded together — an Android client, an
RPM server and a locally built desktop client are all in the wild at different
versions. Both directions must keep working.

Three properties make additive change safe:

1. **Structs go on the wire as name-keyed CBOR maps.** A peer that does not know
   a field skips it by key. This is what makes appending a field safe, and it is
   asserted directly by a test rather than assumed — under a positional
   encoding, appending a field would silently misalign every field after it on
   an old peer instead of failing loudly.
2. **Unknown fields are ignored.** No struct uses `deny_unknown_fields`. New
   client → old server works because the extra field is skipped.
3. **New fields carry `#[serde(default)]`.** Old client → new server works
   because the missing field takes its default.

### Choosing the default is the dangerous part

The default is a compatibility decision, not a formality — it is what an older
peer is *assumed* to have meant.

`supported_codecs` (video) defaults to `all_preferred()`: a client old enough to
omit it predates negotiation, when every codec was already supported.

`supported_audio_codecs` defaults to `legacy_default()` — **Opus only**, not
"everything". A client old enough to omit that field predates *audio*
negotiation and can decode nothing else. Defaulting to "everything" would let
the server pick a newly added codec for a client that cannot decode it.

That bug would not appear until the second audio codec was added, long after the
line was written, and would present as audio that connects and plays silence.
The distinction is pinned by a test that simulates exactly that future.

### Adding a field safely

1. Append it (order does not matter for maps, but appending keeps diffs clean).
2. Add `#[serde(default)]`, or `#[serde(default = "path::to::fn")]` when the
   zero value is wrong.
3. Ask what an *older peer omitting this field actually meant*, and make the
   default say that — not what is convenient today.
4. Add a round-trip test in both directions: a struct without the field
   deserialising into the new one, and the new one deserialising into a struct
   without it.
5. Leave `PROTOCOL_VERSION` alone. It is for changes that genuinely break the
   above — removing or repurposing a field, changing framing, or changing a
   variant's meaning.

## Server-side session state

Not wire format, but the same data seen from the other side. Persistent
sessions are recorded under `$XDG_RUNTIME_DIR/termland/sessions/<id>.session`
as plain key-value text, so a restarted server can find compositors that
outlived it.

| Field | Notes |
|---|---|
| `session_id` | Matches `SessionReady.session_id` |
| `compositor_pid` | Detached compositor, session-group leader after `setsid` |
| `wayland_display` | Socket name to reconnect to |
| `mode`, `width`, `height`, `created_at_unix`, `audio` | Populate `SessionInfo` |
| `owner` | PAM-authenticated creator, or `None` without `--auth`; enforces ownership |
| `runtime_dir` | Compositor's **actual** `XDG_RUNTIME_DIR` |

`runtime_dir` is recorded rather than recomputed because under session isolation
it is the *target user's* `/run/user/<uid>`, which differs from the server
process's own. A later connection has no other way to know whether the session
was isolated.

Compositors are deliberately `setsid`-detached so they survive the server
exiting — that is what makes sessions resumable. The corollary is that killing a
server does **not** clean up its sessions; they must be closed through the
registry (`termland-server --close-session <id>`).
