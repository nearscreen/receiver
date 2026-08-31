# Nearscreen wire protocol v1

Phone (client) opens one TCP connection to a receiver (server). Default port
**9913**. Receivers advertise themselves via Bonjour as `_nearscreen._tcp`;
the client falls back to a configured host:port when nothing is found.
All integers are big-endian.

## Handshake

Client → server, first bytes on the connection:

```
"NSC1"  u32 len  JSON(len bytes)
```

```json
{"v":1,"id":"<identifierForVendor>","model":"iPhone12,1","ios":"26.5",
 "name":"iPhone","w":828,"h":1792,"codec":"h264","app":"0.1.0"}
```

`id` is stable for this app on this phone, `w`/`h` are the native screen size in
pixels, `codec` is what the phone would use if the receiver has no preference.

Server → client reply:

```
"NSS1"  u32 len  JSON(len bytes)
```

```json
{"ok":true,"name":"Home PC","fps":30,"bitrate":8000000,
 "keyframe_interval_s":2,"codec":"h264","scale":1.0}
```

| field | meaning |
|-------|---------|
| `ok` | `false` + `error` → the server closes the connection; the client retries with backoff |
| `name` | receiver's display name, shown on the phone |
| `fps`, `bitrate`, `keyframe_interval_s` | encoder settings the client applies |
| `codec` | `"h264"` or `"hevc"` — the receiver decides what it can decode |
| `scale` | encode at this fraction of the native screen size (1.0 = native) |

Every field except `ok` is optional; the client keeps its own default for
anything the receiver omits.

## Records

After the handshake both directions use the same 16-byte record header:

```
u8  type   u8 flags   u16 reserved(0)   u32 payload_len   u64 pts_us   payload
```

`pts_us` is the phone's host clock in microseconds (monotonic, same base as
ReplayKit sample timestamps).

| type | dir | payload | notes |
|------|-----|---------|-------|
| 0x01 video | c→s | one Annex-B access unit | flags bit0 = keyframe; keyframes carry the parameter sets (SPS+PPS, or VPS+SPS+PPS for HEVC) |
| 0x02 heartbeat | c→s | empty | sent when no video for ~1 s (static screen) |
| 0x03 config | c→s | JSON | what the encoder actually produces — sent before the first access unit and whenever the encoder is re-created |
| 0x04 stats | c→s | JSON | reserved |
| 0x05 log | c→s | UTF-8 text | free-form line for the receiver's log |
| 0x10 request_keyframe | s→c | empty | client forces an IDR on the next frame |
| 0x11 set_params | s→c | JSON | applied live |

`0x03 config` payload — the receiver (re)configures its decoder from it:

```json
{"codec":"h264","w":828,"h":1792,"fps":30,"bitrate":6000000}
```

`0x11 set_params` payload — same fields as the handshake reply, all optional:

```json
{"fps":30,"bitrate":6000000,"keyframe_interval_s":2,"codec":"h264","scale":1.0}
```

A `codec` or `scale` change re-creates the encoder, so the next `0x03 config`
and a keyframe follow.

## Client behaviour

* Encoder: hardware H.264 Main by default, native screen resolution, no
  B-frames, keyframe every 2 s, ~8 Mbit/s. Frames are only produced when the
  screen changes (ReplayKit behaviour) — receivers must not treat silence as
  loss; heartbeats keep the link observable.
* Backpressure: with more than ~3 MB in flight, non-keyframes are dropped and a
  keyframe is requested, so the extension never buffers unboundedly.
* Reconnect: on any error the client rediscovers and reconnects with 1→5 s
  backoff, resends HELLO, and starts with a keyframe.
* Screen lock or an incoming call ends the broadcast (iOS behaviour); the
  broadcast is not resumed automatically.
