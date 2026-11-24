# Research notes

Facts collected on 2026-11-23 before writing the plugin. Line numbers point to
the sources listed below. Anything marked "unconfirmed" was read in code but
not run.

## Sources and versions

- CLIProxyAPI (CPA): https://github.com/router-for-me/CLIProxyAPI, branch main,
  commit 0d25184 (2026-11-23). The local desktop app runs core v8.0.80.
- EasyCLIProxyAPI desktop app v0.3.117 (Tauri, id `com.cpa.gui`), read from
  strings in its program file only.
- pi-web: https://github.com/agegr/pi-web, version 0.18.0, commit 432366b, MIT.
- models.dev: https://models.dev/api.json, fetched 2026-11-23: 6,920,640 bytes,
  326 providers, 10,211 models. MIT.
- Similar plugins (MIT):
  - https://github.com/LIEGU0317/cpa-plugin-model-meta-sync
  - https://github.com/moxi000/models-cache-override (commit 92409df)

## Goal

Fill missing model details in CPA config from models.dev, like pi-web's
"Fill model details" button: the user opens a page, sees a preview, and saves.

## CPA plugin basics

- A plugin is a native library with a C ABI. Calls are JSON messages:
  `cliproxy_plugin_call(method, request)` returns `{ok, result, error}`. The
  host passes a `host->call(ctx, method, request)` function for callbacks.
  Example in Rust: `examples/plugin/management-api/rust/src/lib.rs`.
- Plugin id = DLL file name without extension, optionally followed by
  `-v<version>` (`internal/pluginhost/platform.go:17`, `:62-96`).
  Id pattern: `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`.
- Windows folder: `plugins/windows/amd64`, then `plugins`
  (`platform.go:313-318`). Enable with `plugins.enabled: true` and
  `plugins.configs.<id>.enabled: true`.
- `plugin.register` must return metadata (Name, Version, Author,
  GitHubRepository) and at least one capability (`host.go:1459-1498`).
- Capabilities (`internal/pluginhost/rpc_schema.go`): model_registrar,
  model_provider, model_router, thinking_applier, management_api,
  request/response interceptors and translators, usage_plugin, scheduler,
  executor, auth_provider, command_line_plugin.
- The plugin's own settings arrive as YAML bytes in `config_yaml` at register
  and at every reconfigure (`host.go:1364-1419`). There is no callback to read
  or write the main CPA config.
- Host callbacks (`internal/pluginhost/host_callbacks.go:147-190`):
  `host.http.do` (+ stream variants), `host.model.*`, `host.stream.*`,
  `host.log`, `host.auth.*`, `host.affinity.lookup`,
  `host.routing.reset_cooldown`.
- `host.http.do` request: `{method, url, headers, body(base64),
  host_callback_id}`. It uses CPA's HTTP client and proxy settings
  (`host_callbacks.go:200-215`). Response fields follow
  `pluginapi.HTTPResponse`: `StatusCode`, `Headers`, `Body` (base64).

## Management pages and routes

- `management.register` returns
  `{"routes":[{Method,Path,Menu,Description}],"resources":[{Path,Menu,Description}]}`.
- Routes live under `/v0/management/` and need the management key.
- Resources are GET pages under `/v0/resource/plugins/<id>/`, get a menu entry,
  and are NOT authenticated (`internal/pluginhost/management.go:296-338`).
- Both arrive as `management.handle` with `pluginapi.ManagementRequest`:
  `{Method, Path, Headers, Query, Body(base64), host_callback_id}`. The caller's
  headers are passed through (`management.go:267`, `:315`), so the plugin sees
  the `Authorization` header that the page sent.
- Answer with `pluginapi.ManagementResponse`: `{StatusCode, Headers,
  Body(base64)}`.

## Where the page gets the management key

- Desktop app: plugin pages open in a separate webview on the CPA origin. An
  init script stores the key in `localStorage["cli-proxy-auth"]` as plain JSON
  `{"state":{"managementKey":"..."}}`. It does not set `isLoggedIn`.
- Web panel (management.html): same key name, but the value may be stored as
  `enc::v1::` or `enc::v2::` (XOR with a fixed mask, then base64). See
  `models-cache-override/web/index.html:56-76` for a decoder.
- Fallback: a key field in the page, held in memory only.

## Saving config through CPA

- Generic v8 settings API: `GET|PUT|PATCH|DELETE /v8/management/config/<path>`
  (`internal/api/server_management_v8.go:27`,
  `internal/api/handlers/management/config_v8.go`).
- GET returns JSON for that node with an `ETag` = SHA-256 of the config file.
  Provider API keys are returned in full (only TURN secrets are hidden).
- PUT replaces the node; lists are replaced whole (`config_v8.go:147`).
  With `If-Match`, CPA answers 412 if the file changed since the read
  (`config_v8.go:38-43`, `:241-252`). The tag is a hash of the whole file, so
  one Save should write `api-keys` once.
- CPA validates before writing, keeps comments, and reloads in the background
  after answering (`config_v8.go:272-276`). A save made from inside a plugin
  call therefore should not wait for the plugin's own reconfigure (unconfirmed
  until tested).
- The desktop app saves the same way: `PUT /v8/management/config/api-keys/<type>`.

## What CPA config can hold per model

- `openai-compatibility` models: `name`, `alias`, `display-name`,
  `max-context-length`, `input-modalities`, `output-modalities`,
  `thinking.levels`, plus `image` (this one enables /v1/images, it is NOT image
  input) and protocol flags (`config.example.yaml:1046-1057`).
- `codex` API-key models take fewer fields; there is no input-modalities field
  (`internal/config/config_types.go:962`, unconfirmed for every field).
- Not storable: max output tokens, tool-calling support.
- Thinking level names in CPA (`internal/thinking/types.go:47-62`): none, auto,
  minimal, low, medium, high, xhigh, max.

## Where the values are used

- `thinking.levels` changes behavior: CPA validates and clamps requested
  thinking to these levels (`sdk/cliproxy/auth/api_key_model_capabilities.go:367-457`).
  For openai-compatibility the default is low, medium, high.
- Codex model list (`?client_version=`): context window, reasoning levels,
  text and image input (`internal/client/codex/models/models.go:389-488`).
- Plain `/v1/models` returns only id, object, created, owned_by.
- A `model_registrar` plugin can add models but cannot override models declared
  in config (`sdk/cliproxy/service_executors.go:611`).

## models.dev data

- Top level: providers. Provider: `id, name, npm, doc, env, api, models`.
  `api` is the provider's base URL (301 of 326 providers have it).
- Model, always: `id, name, attachment, reasoning, tool_call, open_weights,
  release_date, last_updated, modalities.input/output, limit.context,
  limit.output`. Often: `family, knowledge, canonical_model_id, status, type,
  temperature, structured_output, reasoning_options, interleaved, cost`.
- `reasoning_options` entries: `{"type":"effort","values":[...]}`,
  `{"type":"toggle"}`, `{"type":"budget_tokens","min","max"}`. Effort words:
  none, minimal, low, medium, high, xhigh, max (and `default`). An empty list
  means always on with no control.
- Values differ per host. Example: MiniMax M3.1 Flash context is 262144 on
  `nvidia` and 1000000 at the maker.

## pi-web behavior (for comparison)

- Fetches `https://models.dev/api.json`, 1 hour memory cache, 15 s timeout.
- Exact model id match only (case-insensitive). Picks the provider by label,
  then by base URL host, then a 60% consensus across all matches.
- Fills empty fields only: name, reasoning, input types (text/image), context,
  max output, cost. Thinking levels do not come from models.dev there.
- Shows "N fields filled" with Undo; writes only on Save.

## Matching, as built

Every step needs the exact model id (ignoring letter case). There is no fuzzy
matching.

1. Same host: the provider whose `api` host equals the entry's base URL host,
   or is its parent domain. These rows are ticked by default.
2. Model maker: first the id prefix (`maker/model` looks for provider
   `maker`, trying both `model` and the full id). If that finds nothing, the
   most common `canonical_model_id` among all exact-id matches (ignoring
   letter case, ties broken alphabetically). `canonical_model_id` is set on
   86% of models, as `maker/model`. These rows are not ticked.
3. Other host: the first provider, alphabetically, that has the id. These rows
   are not ticked.

Models are not skipped by `type`: the field is missing on 9,749 of 10,211
models, and the id match is exact. The page shows `type` and `status` as
information instead.

A provider without an `api` URL (for example `openai`) can never be a same-host
match.

## Field mapping, as built

A field is filled only when it is empty:

| CPA field | models.dev source | Counts as empty |
|---|---|---|
| display-name | `name` | missing, empty, or equal to the model id (ignoring letter case); skipped when the models.dev name equals the id |
| max-context-length | `limit.context` | missing or 0 |
| input-modalities | `modalities.input`, only `text` and `image`, in that order | missing or empty list |
| thinking.levels | `reasoning_options` effort values in CPA order: none, minimal, low, medium, high, xhigh, max | `thinking` missing, or no levels and no min or max |

- `none` is kept, because CPA uses it to allow turning thinking off. `default`
  is dropped.
- New levels go into the existing `thinking` object, so keys such as
  `zero-allowed` stay.
- `codex` entries get the same fields except input-modalities, which codex
  models do not have.
- Output types are not filled; they are not in the approved field list.
- Shown as information only, never saved: tool support, max output tokens,
  "on or off only" thinking, status, and type.

Values from the model maker or another host are not ticked unless the user
ticks them.

## The page

- The page uses the names of the app's own model form: Display name, Context
  window, Input modalities, Reasoning levels.
- One table, one model per row, grouped by provider entry. Reasoning levels
  sit on their own line under each model, with the tool support and max
  output next to them.
- The preview reply lists, for every model, its four `details`. Each has a
  `state`: `fill` (offered, with a change in `changes`), `set` (the config
  already has a value, sent as `current`), `not_used` (codex input
  modalities) or `no_data` (with a `no_data_reason`: `not_found`,
  `not_listed`, `same_as_id`, `no_thinking`, `on_off_only`). Only these four
  field values leave the config, never keys.
- The list loads once when the page opens. The page never loads again by
  itself after a refused key; it reloads once after a save and once after a
  refused save (HTTP 409).
- Colors, sizes and corner shapes were measured from screenshots of the
  EasyCLIProxyAPI app (light theme). The dark theme follows
  `prefers-color-scheme`, as the app's own plugin toolbar does; its colors
  were not compared with the app.
- Icons: Lucide, ISC License, as inline SVG.

## Rules for code inside the CPA process

- Catch panics at the C boundary; never unwrap host data.
- No background threads or timers.
- Hand out buffers with length equal to capacity.
- Treat config bodies as untyped JSON so unknown fields survive; never log or
  return them, because they hold provider keys.
- Loopback calls go to `127.0.0.1` over plain HTTP. The port comes from the
  `Host` header; Go drops `Host` from the header map, so the page also sends
  its own port (`cpa_port`), and `Origin` and `Referer` are the last fallback.

## Findings on the test copy (2026-11-23 and 2026-11-24)

Tested on a separate CPA copy (core v8.0.80) on its own port, never on the
live CPA.

- Every write shape was accepted and reloaded without warnings: display-name,
  context, input types, thinking levels (including `none`), and codex models
  under a key. Putting the original api-keys back gave a byte-identical
  config.yaml.
- A save made from inside the plugin call works: CPA answers the PUT at once
  and reloads after.
- `PUT` ignores `auth_index`. New keys are added at the end; a replaced key
  keeps its place.
- When CPA refuses the key on a request that has a body, it sets
  `Connection: close` and ends the read, so the client sees a connection reset
  instead of the 401. Preview is therefore a GET with query parameters, so a
  wrong key gets a clear 401 answer.
- CPA answers 412 to `If-Match` before it reads the body. The loopback client
  sends `Expect: 100-continue`, so it gets the 412 instead of a reset.
- Read in CPA's main-branch source, not run (it may differ in v8.0.80): after
  5 failed key attempts from one address, CPA blocks that address for 30
  minutes (HTTP 403, "IP banned due to too many failed attempts"). The count
  also applies to local clients. A successful login resets the count.
- A plugin route gets the query string as `Query`, in the form
  `{"name": ["value"]}`.

## Findings on the live install (2026-11-24)

EasyCLIProxyAPI v0.3.117 with CPA core v8.0.80.

- The app shows a plugin page inside its own window, in a web view of about
  1254 by 780 pixels at a window width of 1535. It puts a small floating
  toolbar (move, back, reload, open in browser) at the bottom right of the
  page, so page controls should stay out of that corner.
- Before the page loads, the app stores the management key in
  `localStorage["cli-proxy-auth"]`; the page picked it up without asking.
- The plugins setting was `enabled: true`, `dir: plugins`. The DLL went into
  `plugins\windows\amd64`. The app's Plugins screen turned it on with
  `PUT /v8/management/config/plugins/configs/<id>/enabled`, and CPA loaded
  it at once, with no restart. CPA loads a copy from a `.shadow` folder;
  replacing the DLL while it is loaded was not tested.
- The app calls the management API about every 15 seconds. Each accepted
  call resets CPA's failed-key count for `127.0.0.1` (read in code).

## Similar plugins

- model-meta-sync copies details from each provider's own `/v1/models` and
  writes config.yaml directly (finds the path from CPA's `-config` flag, keeps
  backups). NVIDIA's `/v1/models` has only id and owner, so it cannot fill
  NVIDIA models.
- models-cache-override patches the Codex model cache; its page reads the key
  from the web panel's localStorage and requires `isLoggedIn`.
