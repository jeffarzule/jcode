# GPT-6 Astra and GPT-5.6 harness

This fork improves the existing native OpenAI Responses harness for
`gpt-6-astra`, `gpt-5.6-sol`, `gpt-5.6-terra`, and `gpt-5.6-luna`.
Model selection remains explicit; unknown future families keep their existing
request behavior until their capabilities are verified.

## Behavior

- Modern models can return multiple function calls in one response. Each call
  still passes through jcode's existing permissions, hooks, and execution loop.
  The `batch` tool runs independent work concurrently; grouped function calls
  alone do not make the executor concurrent.
- API-key requests use `prompt_cache_options = { "ttl": "30m" }`. Legacy
  `JCODE_OPENAI_PROMPT_CACHE_RETENTION` settings apply only to older models.
  OAuth requests continue to omit API-only cache parameters. WebSocket
  continuation preserves these settings and the parallel-call setting.
- Astra exposes `low`, `medium`, `high`, `xhigh`, and `max`. Old `none` or
  `minimal` settings migrate to `low`, including saved sessions and model
  switches. Supported explicit effort choices are preserved. jcode's `swarm`
  modes continue to use the highest effort advertised for the selected model.
- Astra and GPT-5.6 Sol, Terra, and Luna use their documented 1,050,000-token
  context fallback. Cached endpoint or account limits take precedence,
  including the agent's compaction budget.
- The system prompt emphasizes finishing authorized work, bounded verification,
  concise context, independent read batching, and retaining the original task
  when the user adds corrections.

Existing encrypted reasoning replay, native compaction, retry handling, and
output-token defaults are retained. The cache TTL matches the API default; it
does not by itself establish a cost improvement.

## Try this build

Use Rust 1.91 or newer. These commands pin the compiler so dependency-local
toolchain files cannot override it. The minimal build excludes optional PDF,
embeddings, and Bedrock features:

```bash
rustup toolchain install 1.91.0 --profile minimal --component rust-src
RUSTUP_TOOLCHAIN=1.91.0 cargo build --locked --no-default-features --bin jcode
```

Use your existing OpenAI subscription login and a separate socket when trying
the new binary, so the shared daemon does not serve an older build:

```bash
ASTRA_SOCKET_DIR="$(mktemp -d /tmp/jcode-astra.XXXXXX)"
./target/debug/jcode --no-update --no-selfdev \
  --provider openai --model gpt-6-astra --socket "$ASTRA_SOCKET_DIR/agent.sock"
```

Use `--provider openai-api` instead when you intentionally want API-key billing.
Select the reasoning effort with `/effort`, or set
`[provider].openai_reasoning_effort` in your jcode config. The default remains
`low`; higher effort is a task-dependent latency and quality choice.

## Reproduce verification

```bash
RUSTUP_TOOLCHAIN=1.91.0 cargo test --locked -p jcode-provider-core -p jcode-provider-openai \
  -p jcode-provider-openai-runtime --lib --tests
RUSTUP_TOOLCHAIN=1.91.0 cargo test --locked --no-default-features --test astra_harness
python3 scripts/test_modern_openai_harness.py target/debug/jcode
```

The offline tests cover modern and legacy request fields, OAuth exclusions,
fragmented and interleaved tool calls, WebSocket continuation, reasoning
migration, persisted session state, and cached compaction limits. The CLI check
owns its temporary daemon, socket, credentials, and files and verifies two
reads, their correlated results, a write, and final completion.

These checks establish harness behavior. Live-model access, task success,
latency, and cost still require evaluation against the intended account and
representative tasks; no benchmark improvement is claimed here.

## Capability sources

Checked against OpenAI documentation on September 4, 2026:

- [GPT-6 Astra model](https://developers.openai.com/api/docs/models/gpt-6-astra)
- [GPT-5.6 Sol model](https://developers.openai.com/api/docs/models/gpt-5.6-sol)
- [GPT-5.6 Terra model](https://developers.openai.com/api/docs/models/gpt-5.6-terra)
- [GPT-5.6 Luna model](https://developers.openai.com/api/docs/models/gpt-5.6-luna)
- [Latest model guide](https://developers.openai.com/api/docs/guides/latest-model)
- [Prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)
- [Parallel function calling](https://developers.openai.com/api/docs/guides/function-calling#parallel-function-calling)
