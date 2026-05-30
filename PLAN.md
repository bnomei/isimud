# isimud — Build Plan

## Brief (agreed)

**isimud** is a macOS menu bar application and MCP server for **text-to-speech (TTS)** —
the functional inverse of [MUNINN](https://github.com/bnomei/muninn) (speech-to-text). It lets
an AI agent *speak*: the agent sends text to an MCP tool, isimud synthesizes and plays it aloud.
Named after Isimud, the messenger god of Enki.

It mirrors the house style of MUNINN (menu bar / tray / config / logging) and
[RAYMON](https://github.com/bnomei/raymon) (`rmcp` streamable-HTTP MCP server).

### Locked decisions

| Decision | Choice |
|---|---|
| MCP port | **3654** (T9 `ENKI`, IANA registered range 1024–49151) |
| MCP transport | **Streamable HTTP** (`rmcp` 1.7), mounted on `axum` at `/mcp` |
| State | **Stateless** — no persistence; MUNINN-style config + tracing only |
| Cloud audio playback | **`rodio`** (decode WAV/MP3 + output via cpal) |
| Apple TTS | **`objc2-avf-audio` `AVSpeechSynthesizer` primary, `say` CLI fallback** |
| `speak` semantics | **Fire-and-forget queue** → returns job id; optional `wait:true` blocks |
| Voice model | **Named `[voices.<name>]`** layer over per-provider config |
| Providers | Apple (local), OpenAI, Google — local-first routing/fallback |
| Menu bar | `tao` + `tray-icon`; **pulse animation while speaking**; headless-capable |

### Voice resolution

- Named voices are the primary abstraction. Each `[voices.<name>]` carries `provider` +
  provider-specific `voice` id (+ optional `rate`/`pitch`/`language`).
- Provider blocks (`[providers.*]`) hold non-voice settings (model, endpoint, language defaults).
- `speak(voice = "name")` → resolves named voice → provider + voice id.
  Plain `speak(text)` uses `[tts].default_voice`.
- If the named provider is unavailable, routing falls through `[tts].providers`.
- `isimud.list_voices` reports configured named voices + raw per-provider voices.

### Config precedence (mirrors MUNINN)

`ISIMUD_CONFIG` → `$XDG_CONFIG_HOME/isimud/config.toml` → `~/.config/isimud/config.toml`.
BYOK provider keys via env (`OPENAI_API_KEY`, `GOOGLE_API_KEY`) with config fallback.

### MCP tools

- `isimud.speak` — enqueue speech; params: `text`, optional `voice`, `rate`, `wait`.
- `isimud.stop` — cancel current/queued speech.
- `isimud.list_voices` — enumerate named + per-provider voices.
- `isimud.status` — current speech state + queue depth.

---

## Checklist

### 1. Scaffold project
- [x] `cargo init` (lib + bin `isimud`, edition 2021)
- [x] `Cargo.toml` package metadata + lib/bin sections
- [x] `rustfmt.toml` (edition 2021, max_width 100, small_heuristics Max, unix)
- [x] `prek.toml` (cargo fmt + clippy hooks)
- [x] Add dependencies via `cargo add`
- [x] Module skeleton compiles (`cargo check` + fmt + clippy clean)
- [x] `configs/config.sample.toml` baseline
- [x] `PLAN.md` (this file)

### 2. Config + logging
- [x] `AppConfig` (TOML, serde, `deny_unknown_fields`)
- [x] `resolve_config_path()` precedence + `write_default_config`
- [x] `[server] [tts] [voices.*] [providers.*] [app] [logging]` sections
- [x] `launchable_default()`
- [x] `.env` loading (dotenvy) + env overrides + secrets resolver
- [x] `init_logging` (stderr fmt + per-target oslog), `RUST_LOG` targets

### 3. Core types + voice resolution
- [x] `SpeakRequest`, `ResolvedSpeech` (provider + voice + rate/pitch/volume)
- [x] Named-voice resolution with `default_voice` + provider fallback order
- [x] `SpeechState` (Idle / Speaking{job}) + event bus (broadcast)
- [x] Unit tests for resolution

### 4. Speech worker (queue + jobs)
- [x] Single serialized worker task + queue (`VecDeque` + `Notify`)
- [x] Job ids (uuid); fire-and-forget enqueue; optional wait-until-done
- [x] stop/cancel; emits `SpeechEvent`s

### 5. Audio playback (rodio)
- [x] Playback sink for cloud WAV/PCM/MP3 bytes (rodio 0.22 `DeviceSink` + `play`)
- [x] Device lifecycle; shared state/pulse with Apple self-playing path

### 6. TTS providers
- [x] `TtsProvider` trait + routing/fallback (`ProviderRegistry::select`)
- [x] Apple: `say` synthesis (cancellable) + native `AVSpeechSynthesis` voice catalog (macOS cfg)
- [x] OpenAI: `/v1/audio/speech`, `gpt-4o-mini-tts`, wav
- [x] Google: `text:synthesize?key=`, LINEAR16
- [x] BYOK env keys + structured diagnostics

### 7. MCP server + HTTP
- [x] `rmcp` handler: `speak` / `stop` / `list_voices` / `status` (+ annotations)
- [x] Event forwarder (speech notifications) + peer management
- [x] `StreamableHttpService` + `LocalSessionManager` mounted on axum `/mcp`
- [x] Loopback bind-guard + optional auth token; graceful shutdown broadcast

### 8. Menu bar + autostart
- [x] `tao` event loop + `tray-icon`; pulse while Speaking, idle otherwise
- [x] `--headless` / `[app].menubar = false` to skip tray
- [x] `runtime_shell` wiring server + worker + tray
- [x] macOS LaunchAgent autostart sync

### 9. Tests, docs, scripts
- [x] Unit tests (voice resolution, config, tool schemas, provider rate mapping, bind guard)
- [x] README, prek hooks (`prek.toml`)
- [x] packaging/build scripts mirroring MUNINN/RAYMON

## Implementation notes

- Apple speech is produced via the macOS `say` CLI (cancellable child process, headless-safe,
  no run loop required); `objc2-avf-audio` `AVSpeechSynthesisVoice` provides the native voice
  catalog for `list_voices`. In-process `AVSpeechSynthesizer` playback can be layered on later
  behind the main-thread run loop.
- `[tts].rate` is a neutral multiplier (1.0 = normal): Apple maps it to `say -r` WPM, OpenAI to
  `speed`, Google to `speakingRate`.
- Effective `volume` is applied in the shared rodio playback path (cloud providers) via
  `Player::set_volume`; the Apple `say` path cannot apply volume/pitch and logs this once.
- `[tts].max_queue_depth` bounds queued jobs (0 = unbounded); a full queue yields a server-defined
  MCP error (code `-32010`). `[tts].wait_timeout_secs` bounds `wait=true` calls (0 = wait forever);
  on timeout the job stays queued and `speak` returns `outcome = "timeout"`.
- Provider/voice fallback is silent to agents (unchanged MCP responses) but logged on the
  `provider` target. A panicking job is isolated per-job, sets a `degraded` health flag (surfaced in
  `isimud.status` and the tray tooltip) and broadcasts `SpeechEvent::Degraded`; the worker keeps
  draining. The runtime supervises the worker handle and marks `degraded` if it exits unexpectedly
  (no auto-restart).
