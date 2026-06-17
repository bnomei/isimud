# isimud

[![Crates.io Version](https://img.shields.io/crates/v/isimud-text-to-speech)](https://crates.io/crates/isimud-text-to-speech)
[![CI](https://img.shields.io/github/actions/workflow/status/bnomei/isimud/ci.yml?branch=main)](https://github.com/bnomei/isimud/actions/workflows/ci.yml)
[![Crates.io Downloads](https://img.shields.io/crates/d/isimud-text-to-speech)](https://crates.io/crates/isimud-text-to-speech)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://flat.badgen.net/badge/discord/bnomei?color=7289da&icon=discord&label)](https://discordapp.com/users/bnomei)
[![Buymecoffee](https://flat.badgen.net/badge/icon/donate?icon=buymeacoffee&color=FF813F&label)](https://www.buymeacoffee.com/bnomei)

**isimud** is a macOS menu bar app and [MCP](https://modelcontextprotocol.io) server that lets AI agents **speak**. An agent sends text to an MCP tool; isimud resolves a named voice, synthesizes it through a local or cloud provider, and plays it aloud through a single serialized speech queue.

It is the functional inverse of [muninn](https://github.com/bnomei/muninn) (speech-to-text). Where muninn turns your voice into text for an agent, isimud turns an agent's text into voice. Named after Isimud, the two-faced messenger god of Enki.

isimud is:
- a local macOS menu bar app whose tray pulses while speaking, with a `--headless` mode for a pure server
- an MCP streamable-HTTP server exposing `speak`, `stop`, `list_voices`, and `status` tools to any MCP client
- BYOK by design: you bring provider keys; isimud routes across Apple local TTS, OpenAI, and Google with local-first fallback

## What isimud Does

High-level flow:

`agent calls isimud.speak -> resolve named voice -> pick first available provider -> synthesize audio -> enqueue on serialized speech worker -> play aloud -> broadcast lifecycle events`

A single speech worker plays one utterance at a time. `speak` is fire-and-forget by default (returns a job id immediately) or blocking (`wait=true`). Every job transition is broadcast as a `SpeechEvent` to the tray (for the pulse animation) and to connected MCP peers (as notifications).

The current app supports:
- a live menu bar tray that pulses while speaking, or headless server-only operation
- an MCP streamable-HTTP endpoint on `127.0.0.1:3654/mcp` (3654 = T9 for *ENKI*)
- named voices that hide provider-specific voice identifiers behind friendly names
- ordered provider routing with availability checks and silent fallback (with loud logging)
- a bounded, serialized speech queue with backpressure and degraded-health supervision
- macOS LaunchAgent autostart

## MCP Tools

| Tool | Description | Key params |
|------|-------------|------------|
| `isimud.speak` | Enqueue speech; returns a `job_id` and `queue_depth`. | `text`, optional `voice`, `rate`, `wait` |
| `isimud.stop` | Cancel the active utterance and clear the queue. | — |
| `isimud.list_voices` | List configured named voices plus per-provider voices. | — |
| `isimud.status` | Report speech state, active job, queue depth, and health. | — |

`isimud.speak` returns `{ job_id, queue_depth }`. With `wait=true` it also returns an `outcome` (`completed` / `failed` / `cancelled` / `timeout`) and, on failure, an `error`. When the queue is full the call is rejected with JSON-RPC error code `-32010` and a `{ queue_depth, capacity }` data payload.

`isimud.status` returns `{ state, job_id?, voice?, provider?, queue_depth, degraded }`, where `state` is `idle` or `speaking` and `degraded` flags that the speech subsystem needs attention.

Speech lifecycle events are forwarded to connected MCP peers as `isimud/speech_event` custom notifications. Event variants: `enqueued`, `started`, `finished`, `failed`, `stopped`, and `degraded`.

## Named Voices & Providers

Named voices are the primary abstraction. A `[voices.<name>]` block maps a friendly name to a provider plus that provider's voice id, so agents never deal with provider-specific identifiers.

```toml
[voices.default]
provider = "apple"
voice = "Samantha"

[voices.narrator]
provider = "openai"
voice = "onyx"

[voices.googler]
provider = "google"
voice = "en-US-Neural2-C"
language = "en-US"
```

> **`default_voice` is a voice name, not a provider name.** `[tts].default_voice` (and the `voice` argument to `isimud.speak`) must match one of the `[voices.<name>]` keys above — e.g. `default`, `narrator`, or `googler`. Setting `default_voice = "openai"` fails with `unknown voice 'openai'` unless a `[voices.openai]` block exists. To make OpenAI the default for a plain `speak(text)`, point `default_voice` at a voice whose `provider = "openai"` (such as `narrator`), or add your own `[voices.<name>]` block mapped to that provider.

Providers and their routing:
- **Apple** — local, no API key. Synthesizes through the macOS `say` CLI (cancellable, headless-safe) and enumerates installed voices natively via `AVSpeechSynthesisVoice`. It plays its own audio, so it honors `rate` but **cannot apply volume or pitch** (logged once when requested).
- **OpenAI** — `POST /v1/audio/speech`; set `OPENAI_API_KEY`.
- **Google** — Google Cloud TTS `text:synthesize`; set `GOOGLE_API_KEY`.

`[tts].providers` is the availability / fallback order. When a named voice's provider is unavailable (for example a missing key), isimud falls back to the next available provider with a loud log line rather than failing silently.

### OpenAI voices

Set the `voice` of a `provider = "openai"` block to one of the following ids. Styles are approximate character descriptions:

| Voice | Style |
| --- | --- |
| `alloy` | neutral, balanced |
| `ash` | clear, articulate |
| `ballad` | smooth, melodic |
| `cedar` | warm, grounded |
| `coral` | vibrant, warm |
| `echo` | precise, resonant |
| `fable` | expressive, storyteller |
| `marin` | natural, conversational |
| `nova` | energetic, bright |
| `onyx` | deep, authoritative |
| `sage` | calm, measured |
| `shimmer` | cheerful, light |
| `verse` | poetic, expressive |

## BYOK & Provider Setup

isimud loads `./.env` from the current working directory by default (disable with `ISIMUD_LOAD_DOTENV=0`). Shell environment variables win over `.env` and config values.

| Concern | Variables | Notes |
| --- | --- | --- |
| Apple TTS | none | Local macOS `say` + `AVSpeechSynthesisVoice`. No key required. |
| OpenAI TTS | `OPENAI_API_KEY` | Falls back to `[providers.openai].api_key`. Endpoint and model are configurable. |
| Google TTS | `GOOGLE_API_KEY` | Falls back to `[providers.google].api_key`. Endpoint and language are configurable. |
| MCP auth | `ISIMUD_AUTH_TOKEN` | Optional bearer token; falls back to `[server].auth_token`. Required for remote binds. |
| Config path | `ISIMUD_CONFIG` | Overrides the default config path resolution. |

## Quick Start

### 1) Build the app

```sh
cargo build --release
```

### 2) Resolve the config path

Config file precedence:
- `ISIMUD_CONFIG`
- `$XDG_CONFIG_HOME/isimud/config.toml`
- `~/.config/isimud/config.toml`

On first run isimud writes a launchable default config to the resolved path. To start from the sample explicitly:

```sh
cp configs/config.sample.toml ~/.config/isimud/config.toml
```

### 3) Set provider env vars (optional)

```sh
export OPENAI_API_KEY="sk-..."   # only if you use the openai provider
export GOOGLE_API_KEY="..."      # only if you use the google provider
```

Apple voices work with no keys at all.

### 4) Run

```sh
./target/release/isimud            # menu bar tray + MCP server
./target/release/isimud --headless # MCP server only
```

Point your MCP client at `http://127.0.0.1:3654/mcp` and call `isimud.speak`.

## Configuration

See [`configs/config.sample.toml`](configs/config.sample.toml) for the full schema.

```toml
[tts]
providers = ["apple", "openai", "google"] # availability / fallback order
default_voice = "default"                  # a [voices.<name>] key below, NOT a provider name
rate = 1.0                                  # neutral multiplier (1.0 = normal)
max_queue_depth = 64                        # jobs allowed behind the active one; 0 = unbounded
wait_timeout_secs = 0                       # seconds wait=true blocks; 0 = wait forever

[server]
host = "127.0.0.1"
port = 3654
path = "/mcp"
allow_remote = false                        # non-loopback bind requires an auth token

[indicator.colors]
idle = "#636366"                            # systemGray (matches muninn)
speaking_bright = "#30D158"                 # systemGreen pulse, bright phase
speaking_dim = "#208A3A"                    # ~66% systemGreen, off phase
```

The bind address is loopback-only unless `[server].allow_remote = true` **and** an auth token is set.

### Live config reload

isimud watches the config file and applies changes without a restart. Saving the file hot-swaps
the running configuration via an `ArcSwap`, so tray icon colors (`[indicator.colors]`, validated as
`#RRGGBB` hex), named voices, rates, and provider credentials all take effect on the next utterance.
Invalid edits are rejected with a logged warning and the previous config is kept.

## Backpressure & Observability

isimud serializes all speech through one worker and bounds the queue:
- `[tts].max_queue_depth` caps jobs waiting behind the active utterance. When full, `isimud.speak` is rejected with JSON-RPC code `-32010` and a `{ queue_depth, capacity }` payload. Set `0` for unbounded fire-and-forget.
- `[tts].wait_timeout_secs` bounds how long a `wait=true` call blocks; `0` waits forever. On timeout the result `outcome` is `timeout`.
- A supervisor marks the engine **degraded** if the speech worker panics or exits unexpectedly, broadcasting a `degraded` event and surfacing `degraded: true` in `isimud.status`.
- Lifecycle events (`enqueued` / `started` / `finished` / `failed` / `stopped` / `degraded`) are broadcast to the tray and to MCP peers.

Tracing logs go to stderr and are controlled with `RUST_LOG` (targets: `runtime`, `server`, `provider`, `config`, `speech`). Set the base level with `[logging].level`.

## Speech Provider Operational Expectations

These are current operational assumptions, not enforced runtime limits. isimud validates that `text` is non-empty, then forwards the utterance to the selected provider without imposing a fixed character, byte, or token cap.

- **Expected text size:** `isimud.speak` is intended for short agent utterances such as status updates, confirmations, and concise paragraphs. Longer passages may work, but latency, provider rejection risk, and returned audio size grow with input length.
- **Provider timeout behavior:** local Apple speech runs until `say` completes or the job is cancelled. Cloud provider calls rely on provider/client/network behavior; isimud does not currently add a separate synthesis request timeout. For blocking MCP calls, `[tts].wait_timeout_secs` only limits how long `wait=true` waits for the queued job outcome; it does not cancel synthesis when the wait result times out.
- **Response-size risk:** cloud providers return audio bytes that are decoded and played by the shared playback path. Very long utterances can produce large responses, increasing memory use and decode/playback time. No maximum response byte size is enforced today.
- **Queue behavior:** all speech is serialized through one worker. The active job is not counted in `[tts].max_queue_depth`; that setting caps only jobs waiting behind it, and `0` keeps the waiting queue unbounded.
- **Rate assumptions:** speech `rate` is treated as a neutral multiplier where `1.0` is normal. Providers may interpret or clamp rates differently, and isimud does not enforce a provider-specific rate policy beyond resolving the configured/requested value.

## Packaging (macOS)

```sh
TARGET=aarch64-apple-darwin scripts/build-release.sh
scripts/package-macos-app.sh   # builds dist/Isimud.app (+ .zip)
```

Enable login autostart with `[app].autostart = true`; isimud writes a LaunchAgent using the current executable path. When running the packaged `.app`, prefer macOS Login Items over the raw LaunchAgent.

## Local Pre-commit

This repo ships a native `prek.toml` for fast local gates before you commit.

```sh
prek run --all-files
prek install
```

The hooks stay intentionally small: `cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings`.

## Current Limits

- isimud currently supports macOS only.
- The Apple `say` provider honors `rate` but cannot apply `volume` or `pitch`.
- Provider fallback is silent substitution with loud logging; there is no per-call provider pinning beyond the named voice.
- The MCP server is streamable-HTTP only; there is no stdio transport.

## License

MIT — see [LICENSE](LICENSE).
