# Fill Model Details

A plugin for CLIProxyAPI (CPA). It fills empty model details in your CPA
config with data from [models.dev](https://models.dev): display name, context
window, input types (text, image), and thinking levels.

Nothing changes until you press a button. The page first shows a preview, and
you pick which rows to save.

The idea comes from the "Fill model details" button in
[pi-web](https://github.com/agegr/pi-web).

## What it fills

The plugin reads the model lists of your `openai-compatibility` and `codex`
API-key entries. For each model it fills a field only when the field is empty:

| CPA field | Value from models.dev | Counts as empty |
|---|---|---|
| `display-name` | model name | missing, empty, or the same as the model id |
| `max-context-length` | context window | missing or 0 |
| `input-modalities` | input types, only `text` and `image` | missing or empty |
| `thinking.levels` | thinking effort levels | no thinking levels and no min or max |

- `codex` models have no input types field, so they never get one.
- Fields that already have a value are never changed. All other keys in your
  config stay as they are.
- Tool support, max output tokens, model status, and model type are shown on
  the page as information. CPA config has no place to store them.

## How it finds a model on models.dev

The model id must match exactly (letter case does not matter). There is no
fuzzy matching.

1. **Same host**: a models.dev provider with the same host as your entry's
   base URL. These rows are ticked by default.
2. **Model maker**: the maker named in the id (for example `moonshotai` in
   `moonshotai/kimi-k3.1-pro`), or the maker that models.dev links the model
   to. These rows are not ticked.
3. **Other host**: any other provider that has the same model id. These rows
   are not ticked.

Values can differ between hosts. For example, one host may give a model a
smaller context window than its maker does. Check the rows that are not
ticked before you tick them.

Thinking levels have their own table, because CPA uses them: it limits the
thinking a request can ask for to the levels saved for the model.

## Install

Tested with CPA core v8.0.80 (in the EasyCLIProxyAPI desktop app v0.3.117) on
Windows x64.

1. Build the plugin (see below).
2. Copy `fill_model_details.dll` into your CPA plugins folder and rename it to
   `fill-model-details.dll`. The file name sets the plugin id.
3. Turn the plugin on in CPA's `config.yaml`:

   ```yaml
   plugins:
     enabled: true
     dir: "C:/path/to/plugins"
     configs:
       fill-model-details:
         enabled: true
   ```

4. Restart CPA.

## Use

1. Open the CPA management panel and choose **Fill Model Details** in the
   plugin menu.
2. Press **Preview**. The first preview downloads about 7 MB from models.dev.
3. Check the rows. Tick or untick them.
4. Press **Save selected**. CPA saves the config and reloads it in the
   background.

The page tries to use the management key from your CPA login. If it finds
none, or CPA does not accept it, the page asks for the key. A typed key stays in the page only and is never saved.

If the config changes between Preview and Save, the save is refused and
nothing is written. Press Preview again to see the current state.

## Limits

- After 5 wrong management keys, CPA blocks your address for 30 minutes.
- The plugin talks to CPA over plain HTTP on `127.0.0.1`. A CPA that only
  serves HTTPS is not supported.
- A models.dev provider without an API address (for example `openai`) can
  never be a same-host match. Its models can still match as model maker
  rows.
- The plugin reads only `openai-compatibility` and `codex` API-key entries.

## Data and privacy

- models.dev data is downloaded through CPA's own HTTP client, so CPA's proxy
  settings apply.
- A copy is kept in `%LOCALAPPDATA%\cpa-plugin-fill-model-details\` (on other
  systems: `$XDG_CACHE_HOME/cpa-plugin-fill-model-details/` or
  `~/.cache/cpa-plugin-fill-model-details/`). A copy older than one day is
  downloaded again. **Download fresh models.dev data** on the page downloads
  it at once. Delete the folder to remove the copy.
- Provider API keys never reach the page. The management key is passed on to
  CPA's own settings API for each request and is never stored.

## Build and test

Needs Rust 1.93 or newer.

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

The release build is `target/release/fill_model_details.dll`.

The tests use a small cut of the models.dev data,
`tests/fixtures/models-dev-sample.json`. To make it again from a fresh
download of `https://models.dev/api.json`, run this in `tests/fixtures`:

```sh
uv run --no-project python -I make_fixture.py <api.json> models-dev-sample.json
```

`docs/research.md` has the facts about CPA and models.dev that the plugin is
built on.

## Licence

MIT. See `LICENSE`. The models.dev data and pi-web are also MIT licensed.
