# isimud

[![Crates.io Version](https://img.shields.io/crates/v/isimud-text-to-speech)](https://crates.io/crates/isimud-text-to-speech)
[![CI](https://img.shields.io/github/actions/workflow/status/bnomei/isimud/ci.yml?branch=main)](https://github.com/bnomei/isimud/actions/workflows/ci.yml)
[![Crates.io Downloads](https://img.shields.io/crates/d/isimud-text-to-speech)](https://crates.io/crates/isimud-text-to-speech)
[![License](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Discord](https://flat.badgen.net/badge/discord/bnomei?color=7289da&icon=discord&label)](https://discordapp.com/users/bnomei)
[![Buymecoffee](https://flat.badgen.net/badge/icon/donate?icon=buymeacoffee&color=FF813F&label)](https://www.buymeacoffee.com/bnomei)

**isimud** is a macOS menu bar app and streamable-HTTP [MCP](https://modelcontextprotocol.io) server that lets AI agents speak. An agent sends text to an MCP tool; isimud resolves a named voice, synthesizes speech through Apple, OpenAI, or Google, then plays it through a single serialized speech queue.

isimud is the text-to-speech counterpart to [muninn](https://github.com/bnomei/muninn), which turns speech into text for agents.

Use isimud when you want:

- A local macOS tray app that pulses while an agent is speaking.
- A headless MCP server for scripted or background use.
- Local-first text-to-speech with optional bring-your-own-key cloud providers.
- Named voices that hide provider-specific voice IDs from agents.
- Queueing, cancellation, status, and speech lifecycle notifications over MCP.

## Quickstart

Complete this path to run isimud with the local Apple text-to-speech provider. Apple TTS does not require an API key.

### Prerequisites

- macOS. The project is macOS-only; the packaged app declares macOS 12.0 or newer.
- Rust 1.89 or newer and Cargo.
- An MCP client that supports streamable HTTP.

### Install

Install the published binary crate:

```bash
cargo install isimud-text-to-speech
```

Verify that Cargo installed the `isimud` binary:

```bash
isimud --version
```

Or build from this repository:

```bash
cargo build --release --bin isimud
./target/release/isimud --version
```

### Configure

isimud creates a launchable default config on first run if no config file exists. That generated config contains a single Apple-backed `default` voice. To start from the full sample config instead, create the config directory and copy the sample:

```bash
mkdir -p ~/.config/isimud
cp configs/config.sample.toml ~/.config/isimud/config.toml
```

Config path precedence is:

1. `ISIMUD_CONFIG`
2. `$XDG_CONFIG_HOME/isimud/config.toml`
3. `~/.config/isimud/config.toml`

### Run

Start the menu bar app and MCP server:

```bash
isimud
```

For server-only operation:

```bash
isimud --headless
```

CLI options:

```txt
--headless     Run only the MCP server
-h, --help     Print help
-V, --version  Print the version
```

Expected result:

- In menu bar mode, a small isimud indicator appears in the macOS menu bar.
- The MCP server listens on `http://127.0.0.1:3654/mcp`.
- Calling `isimud.status` from your MCP client returns `state: "idle"` when nothing is speaking.

## Connect an MCP client

Configure your MCP client with streamable HTTP transport:

```txt
URL: http://127.0.0.1:3654/mcp
```

If you set `ISIMUD_AUTH_TOKEN` or `[server].auth_token`, add this request header:

```txt
Authorization: Bearer <TOKEN>
```

The server only binds to loopback IP addresses. `127.0.0.1` and `::1` are accepted; hostnames such as `localhost` and non-loopback addresses such as `0.0.0.0` are rejected at startup.

When auth is configured, requests without the exact bearer token return HTTP `401`.

## Speak from an agent

Call `isimud.speak` with text:

```json
{
  "text": "Build finished.",
  "voice": "default",
  "rate": 1.0
}
```

The call returns immediately by default:

```json
{
  "job_id": "00000000-0000-0000-0000-000000000000",
  "queue_depth": 0
}
```

Set `wait` to `true` when the MCP caller should block until the utterance completes, fails, is cancelled, or reaches `[tts].wait_timeout_secs`:

```json
{
  "text": "Deployment complete.",
  "wait": true
}
```

With `wait=true`, the response includes `outcome`:

```json
{
  "job_id": "00000000-0000-0000-0000-000000000000",
  "queue_depth": 0,
  "outcome": "completed"
}
```

## Configure voices and providers

Named voices are the public voice contract for agents. The `voice` argument to `isimud.speak` and `[tts].default_voice` must match a `[voices.<name>]` key.

```toml
[tts]
providers = ["apple", "openai", "google"]
default_voice = "default"
rate = 1.0
max_queue_depth = 64
wait_timeout_secs = 0

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

Important: `default_voice` is a voice name, not a provider name. To make OpenAI the default, set `default_voice = "narrator"` or create another `[voices.<name>]` block whose `provider = "openai"`.

Optional per-voice fields are `language`, `rate`, `pitch`, and `volume`. A request `rate` overrides the voice `rate`, which overrides `[tts].rate`; voice `volume` defaults to `1.0`.

Provider availability follows `[tts].providers`. If the requested voice's provider is unavailable, isimud selects the first available fallback provider and drops the provider-specific voice ID for that fallback utterance. If no provider is available, the job fails and emits a `failed` event.

### Provider setup

| Provider | Credentials | Behavior |
| --- | --- | --- |
| Apple | None | Uses the macOS `say` command, plays inline, and enumerates installed voices with `AVSpeechSynthesisVoice`. |
| OpenAI | `OPENAI_API_KEY` or `[providers.openai].api_key` | Calls `POST /v1/audio/speech`, requests the configured response format, and plays returned audio through `rodio`. |
| Google | `GOOGLE_API_KEY` or `[providers.google].api_key` | Calls Google Cloud Text-to-Speech `text:synthesize`, decodes `audioContent`, and plays returned audio through `rodio`. |

OpenAI and Google send a rate/speed value only when the resolved rate is within `0.25..=4.0`. Google sends pitch only when the resolved pitch is within `-20..=20`.

OpenAI built-in voice IDs exposed by `isimud.list_voices`:

```txt
alloy ash ballad cedar coral echo fable marin nova onyx sage shimmer verse
```

Google voice listing is intentionally best-effort and currently returns an empty provider catalog; configured named Google voices still work.

## Configuration reference

See [configs/config.sample.toml](configs/config.sample.toml) for the full sample.

| Setting | Default | Description |
| --- | --- | --- |
| `[app].menubar` | `true` | Runs the macOS tray app. `--headless` disables the tray for that process. |
| `[app].autostart` | `false` | Syncs a per-user macOS LaunchAgent that starts the current executable at login. |
| `[server].host` | `"127.0.0.1"` | Loopback bind host. Must parse as an IP address. |
| `[server].port` | `3654` | MCP HTTP port. |
| `[server].path` | `"/mcp"` | MCP streamable-HTTP path. |
| `[server].auth_token` | unset | Optional bearer token. `ISIMUD_AUTH_TOKEN` takes precedence. |
| `[tts].providers` | `["apple", "openai", "google"]` | Provider fallback order. |
| `[tts].default_voice` | `"default"` | Name of the default `[voices.<name>]` entry. |
| `[tts].rate` | `1.0` | Neutral speaking-rate multiplier. |
| `[tts].max_queue_depth` | `64` | Number of jobs allowed behind the active utterance. `0` disables the limit. |
| `[tts].wait_timeout_secs` | `0` | Timeout for `wait=true` calls. `0` waits forever. |
| `[indicator.colors.*]` | Apple system gray/green palette | Menu bar indicator colors. Values must be `#RRGGBB` hex strings. |

The TOML schema is strict. Unknown fields fail config parsing; hot reload keeps the previous config when a changed file fails to parse or validate.

Environment variables:

| Variable | Purpose |
| --- | --- |
| `ISIMUD_CONFIG` | Overrides config path resolution. |
| `ISIMUD_AUTH_TOKEN` | Overrides `[server].auth_token`. |
| `ISIMUD_LOAD_DOTENV=0` | Disables startup loading of `./.env` from the current working directory. Also accepts `false` or `no`. |
| `OPENAI_API_KEY` | Enables the OpenAI provider. |
| `GOOGLE_API_KEY` | Enables the Google provider. |
| `RUST_LOG` | Overrides `[logging].level`; useful targets are `runtime`, `server`, `provider`, `config`, and `speech`. |

Shell environment variables win over `.env` and config-file secrets.

## MCP tool reference

| Tool | Parameters | Result |
| --- | --- | --- |
| `isimud.speak` | `text` required; optional `voice`, `rate`, `wait` | `job_id`, `queue_depth`; with `wait=true`, also `outcome` and optional `error`. |
| `isimud.stop` | none | Cancels the active job if present and clears queued jobs. Returns `cancelled_job` and `cleared`. |
| `isimud.list_voices` | none | Lists configured named voices and provider voice catalogs. |
| `isimud.status` | none | Returns `state`, active `job_id`, `voice`, `provider`, `queue_depth`, and `degraded`. |

`isimud.speak` rejects empty text and unknown named voices as invalid parameters. When the queue is full, it returns JSON-RPC error code `-32010` with this data payload:

```json
{
  "queue_depth": 64,
  "capacity": 64
}
```

Connected MCP peers receive `isimud/speech_event` custom notifications for these lifecycle events:

```txt
enqueued started finished failed stopped degraded
```

Custom MCP requests named `isimud/quit` or `isimud/exit` trigger graceful shutdown.

## Runtime behavior

isimud runs one speech worker. Jobs never overlap; each accepted utterance waits for the current utterance to finish or be cancelled.

The active job does not count toward `[tts].max_queue_depth`. That setting only limits jobs waiting behind the active utterance.

Saving the config file hot-reloads the speech engine and tray palette. Valid changes update voices, provider credentials, speaking rates, queue settings, and tray colors without a restart. Invalid edits are logged and the previous config stays active.

Server bind settings (`[server].host`, `[server].port`, `[server].path`, and auth) and LaunchAgent autostart sync are applied at startup. Restart isimud after changing those settings.

The macOS tray icon is gray while idle and pulses green while speaking. If the speech worker exits unexpectedly or a job panics, `isimud.status` reports `degraded: true` and a `degraded` event is broadcast.

## macOS app features

### URL scheme

The packaged app registers the `isimud://` URL scheme. Use it to enqueue speech from macOS links or automation:

```txt
isimud://speak/Hello%20world
isimud://speak?text=Hello%20world&voice=narrator&rate=1.25
```

The path text takes precedence over the `text` query parameter when both are present. The URL scheme is available when running the packaged `.app` that includes the `CFBundleURLTypes` entry.

### Tray click

In menu bar mode, a left click on the tray icon tries to run the optional `fortune` command and speak its output. If `fortune` is not installed or returns no text, the click is ignored and a warning is logged.

### Autostart

Set `[app].autostart = true` to sync a per-user LaunchAgent at `~/Library/LaunchAgents/com.bnomei.isimud.plist`. The LaunchAgent uses the current executable path and sets `ISIMUD_CONFIG` to the resolved config path.

When running the packaged `.app`, prefer macOS Login Items if you want app-style launch behavior.

## Packaging

Build a release binary for a target:

```bash
TARGET=aarch64-apple-darwin scripts/build-release.sh
```

Build a macOS app bundle and zip archive from an existing release binary:

```bash
TARGET=aarch64-apple-darwin scripts/package-macos-app.sh
```

If you built the default host target with `cargo build --release --bin isimud`, run `scripts/package-macos-app.sh` without `TARGET`.

Useful packaging variables:

| Variable | Default | Description |
| --- | --- | --- |
| `BIN_NAME` | `isimud` | Binary name copied into the app bundle. |
| `PRODUCT_NAME` | `Isimud` | App bundle product name. |
| `BUNDLE_IDENTIFIER` | `com.bnomei.isimud` | macOS bundle identifier. |
| `TARGET` | unset | Selects `target/<TARGET>/release/isimud` as the binary path. |
| `BIN_PATH` | unset | Explicit binary path. Overrides `TARGET`. |
| `ICON_PATH` | `packaging/macos/Isimud.icns` | Optional icon copied into the app bundle when present. |
| `CODESIGN_APP` | `1` | Ad-hoc signs with `codesign --sign -`. Set to `0` if `codesign` is unavailable. |
| `ZIP_APP` | `1` | Set to `0` to skip zip creation. |

Create a tarball release archive from a built target:

```bash
VERSION=$(scripts/resolve-version.sh) TARGET=aarch64-apple-darwin scripts/package-release.sh
```

## Development

Run the main checks:

```bash
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
```

This repository also includes [prek](https://github.com/j178/prek) hooks:

```bash
prek install
prek run --all-files
```

## Current limits

- macOS is the only supported platform.
- The MCP server supports streamable HTTP only; there is no stdio transport.
- The server binds only to loopback addresses.
- Apple `say` honors `rate` but does not apply `volume` or `pitch`.
- OpenAI and Google request timeouts are left to the HTTP client and provider/network behavior. `[tts].wait_timeout_secs` only limits how long an MCP `wait=true` call waits for the job outcome; it does not cancel synthesis.
- isimud validates that `text` is non-empty, but it does not enforce a fixed character, byte, token, or provider response-size limit.
- Cloud-provider audio is decoded and played in memory, so very long utterances can increase memory use and playback latency.

## Source map

- Configuration loading and defaults: [src/config.rs](src/config.rs)
- MCP tools and notification fan-out: [src/mcp.rs](src/mcp.rs)
- HTTP server and loopback/auth enforcement: [src/server.rs](src/server.rs)
- Named voice resolution: [src/voices.rs](src/voices.rs)
- Speech queue and worker: [src/worker.rs](src/worker.rs)
- Provider registry and fallback: [src/providers/mod.rs](src/providers/mod.rs)
- macOS tray behavior: [src/runtime_tray.rs](src/runtime_tray.rs)
- URL scheme parsing: [src/url_scheme.rs](src/url_scheme.rs)
- macOS app bundle template: [packaging/macos/Info.plist.template](packaging/macos/Info.plist.template)

## License

MIT. See [LICENSE](LICENSE).
