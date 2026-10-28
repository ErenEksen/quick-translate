# Translation backends

Quick Translate sends the selected text to one of two services, picked in
**Settings → Backend**:

- **DeepL** (the default)
- **OpenAI-compatible**: any server that implements the chat completions API, such as
  OpenAI, OpenRouter, Ollama, LM Studio or llama.cpp

Switching backends takes effect on the next translation, with no restart.

API keys are stored only in the desktop keyring through the Secret Service API (GNOME
Keyring, KWallet 6, KeePassXC and others). They are saved under the attributes
`application = io.github.quicktranslate.QuickTranslate` and `backend = deepl` or
`backend = openai`. Keys never appear in `config.toml`, in logs or in any other file. If no
Secret Service provider is running, settings tells you and offers to keep the key **for this
session only**. You then have to enter it again after the daemon restarts.

## DeepL

1. Create a DeepL API account. Note that the API is separate from a DeepL Translator
   subscription.
2. Open settings and click **Get API key**. This opens DeepL's API key page in your browser.
3. Copy the key, paste it into the **DeepL API key** field and click **Save key**.

On save, Quick Translate calls DeepL's usage endpoint (`GET /v2/usage`). It either shows the
key as valid along with your character usage, for example `120,000 / 1,000,000`, or it shows
why the key was rejected. The usage figure is also shown each time you open settings.

### Free and Pro keys

- Keys ending in `:fx` are free or Developer keys and are sent to `api-free.deepl.com`.
- All other keys are sent to `api.deepl.com`.

If the chosen host rejects the key with HTTP 403, Quick Translate retries once on the other
host. If that works, it remembers the host in `config.toml` as `deepl.cached-host`, so later
requests go straight to the right one. The key itself is never written there.

### Quota (2026 change)

In 2026 DeepL replaced the monthly free allowance with a **one-time Developer credit of
1,000,000 characters**. It does not reset each month. Once it is used up, DeepL answers with
HTTP 456 and the popup shows **DeepL quota exhausted**. You can then:

- upgrade to DeepL API Pro, or
- switch to the OpenAI-compatible backend, for example a local Ollama model that has no
  quota at all.

The usage line in settings shows how much of the credit is left.

### Errors

| What you see | Cause |
|---|---|
| Invalid API key | HTTP 403 from both hosts |
| DeepL quota exhausted | HTTP 456 |
| Rate limited by the translation service | HTTP 429 that persists after 3 automatic retries with exponential backoff |
| Connection error / The request timed out | Network down, DNS failure, or no response within the timeout |

## OpenAI-compatible

Settings asks for three values:

- **Base URL**: the API root, which normally ends in `/v1`. Quick Translate appends
  `/chat/completions`.
- **Model**: the model name exactly as the server expects it.
- **API key**: optional. Leave it empty for local servers. When it is empty, no
  `Authorization` header is sent. Otherwise the key is sent as `Authorization: Bearer <key>`.

Requests stream the reply (`stream: true`), so the translation fills in as tokens arrive. They
use temperature 0 and a system prompt that asks for the translation only, with line breaks and
formatting kept. This backend always shows the source language as "Auto" and does not detect
a language.

Click **Test connection** to send a short translation request ("Hello" into your target
language). Settings then shows the reply or the endpoint's own error message, such as an
unknown model name.

### Examples

| Service | Base URL | Model (example) | API key |
|---|---|---|---|
| OpenAI | `https://api.openai.com/v1` | `gpt-4o-mini` | required, from platform.openai.com |
| OpenRouter | `https://openrouter.ai/api/v1` | `openai/gpt-4o-mini` | required, from openrouter.ai/keys |
| Ollama | `http://localhost:11434/v1` | `llama3` | empty |
| LM Studio | `http://localhost:1234/v1` | the model loaded in LM Studio | empty |
| llama.cpp server | `http://127.0.0.1:8080/v1` | any name | empty |

For Ollama, pull the model first with `ollama pull llama3` and make sure `ollama serve` is
running. Any model with good multilingual ability works. Smaller models translate faster but
less accurately.

## Transport security and privacy

- Text is sent only to the active backend's endpoint. The only other requests the app makes
  are the DeepL usage check and the **Get API key** page, which opens in your browser.
- There is no telemetry or analytics of any kind.
- HTTPS is required for every host except loopback addresses: `localhost`, `*.localhost`,
  `127.0.0.0/8` and `::1`. Plain `http://` to any other host, such as an Ollama server
  elsewhere on your LAN (`http://192.168.1.20:11434/v1`), is refused until you explicitly
  confirm that you want an unencrypted connection. Your text and API key then cross the
  network in clear text.
- HTTP redirects are never followed, so a request body cannot be forwarded to another host.
- Selected text and translations are never written to disk or logs.
- Requests use a 5-second connect timeout and a total timeout that defaults to 30 seconds and
  can be changed as `request-timeout-secs` in `config.toml`. Closing the popup, or editing the
  source text or a language, aborts the request that is in flight.
