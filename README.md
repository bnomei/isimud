# isimud

**isimud** is a macOS menu bar app and [MCP](https://modelcontextprotocol.io) server that lets
AI agents **speak**. It is the functional inverse of [MUNINN](https://github.com/bnomei/muninn)
(speech-to-text): an agent sends text to an MCP tool and isimud synthesizes and plays it aloud.
Named after Isimud, the messenger god of Enki.

## Features

- **MCP streamable-HTTP server** (`rmcp` 1.7) mounted on `axum` at `/mcp`, port **3654**
  (T9 for *ENKI*).
- **Named voices**: `[voices.<name>]` map a friendly name to a provider + provider voice id, so
  agents never deal with provider-specific identifiers.
- **Providers**: Apple local (`say` + native `AVSpeechSynthesis` voice catalog), OpenAI
  (`/v1/audio/speech`), and Google Cloud TTS, with local-first routing and fallback.
- **Serialized speech queue**: `speak` is fire-and-forget (returns a job id) or `wait=true`.
- **Menu bar tray** that pulses while speaking; runs **headless** with `--headless` or
  `[app].menubar = false`.
- macOS **LaunchAgent autostart** via `[app].autostart`.

## MCP tools

| Tool | Description |
|------|-------------|
| `isimud.speak` | Enqueue speech. Params: `text`, optional `voice`, `rate`, `wait`. |
| `isimud.stop` | Cancel the current utterance and clear the queue. |
| `isimud.list_voices` | List configured named voices + per-provider voices. |
| `isimud.status` | Report current speech state and queue depth. |

Speech lifecycle events are forwarded to connected MCP peers as `isimud/speech_event`
custom notifications.

## Install & run

```sh
cargo build --release
./target/release/isimud            # menu bar + MCP server
./target/release/isimud --headless # MCP server only
```

On first run a default config is written to the resolved config path.

## Configuration

Resolved in order: `ISIMUD_CONFIG` → `$XDG_CONFIG_HOME/isimud/config.toml` →
`~/.config/isimud/config.toml`. See [`configs/config.sample.toml`](configs/config.sample.toml)
for the full schema. Provider keys are BYOK via env (`OPENAI_API_KEY`, `GOOGLE_API_KEY`) with
config fallback; the optional MCP bearer token comes from `ISIMUD_AUTH_TOKEN` or
`[server].auth_token`.

```toml
[tts]
providers = ["apple", "openai", "google"] # availability / fallback order
default_voice = "default"
rate = 1.0                                  # neutral multiplier (1.0 = normal)

[voices.default]
provider = "apple"
voice = "Samantha"

[voices.narrator]
provider = "openai"
voice = "onyx"
```

The bind address is loopback-only unless `[server].allow_remote = true` **and** an auth token
is set.

## License

MIT — see [LICENSE](LICENSE).
