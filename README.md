# Fill Model Details

A plugin for CLIProxyAPI (CPA). It fills empty model details in your CPA
config with data from [models.dev](https://models.dev): display name, context
window, input modalities (text, image), and reasoning levels.

Nothing changes until you press Save. The page lists what it can fill, and
you pick which values to save. It can also reset a model, so that CPA uses
its own defaults for it again.

The idea comes from the "Fill model details" button in
[pi-web](https://github.com/agegr/pi-web).

## What it fills

The plugin reads the model lists of your `openai-compatibility` and `codex`
API-key entries. For each model it fills a field only when the field is empty:

| Name on the page | CPA field | Value from models.dev | Counts as empty |
|---|---|---|---|
| Display name | `display-name` | model name | missing, empty, or the same as the model id |
| Context window | `max-context-length` | context window | missing or 0 |
| Input modalities | `input-modalities` | only `text` and `image` | missing or empty |
| Reasoning levels | `thinking.levels` | thinking effort levels | no levels and no min or max |

The names on the page are the same as in the app's own model form.

- `codex` models have no input modalities field, so they never get one.
- Filling never changes a field that already has a value. All other keys in
  your config stay as they are.
- Tool support, max output tokens, model status, and model type are shown on
  the page as information. CPA config has no place to store them.

## How it finds a model on models.dev

The model id must match exactly (letter case does not matter). There is no
fuzzy matching.

1. **Same host**: a models.dev provider with the same host as your entry's
   base URL. These values are ticked by default.
2. **Model maker**: the maker named in the id (for example `moonshotai` in
   `moonshotai/kimi-k3.1-pro`), or the maker that models.dev links the model
   to. These values are not ticked.
3. **Other host**: any other provider that has the same model id. These
   values are not ticked.

Values can differ between hosts. For example, one host may give a model a
smaller context window than its maker does. Check the values that are not
ticked before you tick them. The page marks them "Model maker" or "Other
host".

Reasoning levels have their own line under each model, because CPA uses them:
it limits the thinking a request can ask for to the levels saved for the
model.

## Install

Tested with CPA core v8.0.80 (in the EasyCLIProxyAPI desktop app v0.3.117) on
Windows x64.

1. Build the plugin (see below).
2. Copy `fill_model_details.dll` into your CPA plugins folder and rename it to
   `fill-model-details.dll`. The file name sets the plugin id. With the
   default setting (`plugins.dir: plugins`), the folder on Windows x64 is
   `plugins\windows\amd64` in CPA's folder.
3. Turn the plugin on. In EasyCLIProxyAPI, use the switch on the **Plugins**
   screen: CPA then loads the plugin at once, without a restart. Without the
   app, set it in CPA's `config.yaml`, and restart CPA if the plugin does not
   show up:

   ```yaml
   plugins:
     enabled: true
     configs:
       fill-model-details:
         enabled: true
   ```

## Use

1. Open **Plugins**, then **Fill Model Details**. The list loads by itself.
   The first time, the plugin downloads about 7 MB from models.dev.
2. Check the values. Tick or untick them: one value, a whole model with the
   box in front of it, or everything with the box at the top.
3. Press **Save**. CPA saves the config and reloads it in the background.

Each model shows its four details. A detail is either offered with a tick
box, marked "Already set", or has no value on models.dev. Models with nothing
to fill are in a folded list under the table.

### Reset a model

Press **Reset** beside a model to give up its saved details. The values are
crossed out and marked "Will be cleared". Nothing changes until you press
**Save**, and a second press on **Reset** takes the mark off.

Save then removes the model's display name, context window, input modalities
and reasoning levels from the config, so CPA uses its own defaults for that
model again. Every other setting of the model stays.

- A reset also removes values you typed by hand. The plugin cannot bring
  those back.
- After a reset, the page offers the models.dev values for that model again.
  They stay unticked while the page is open. When you open the page again,
  values from the same host are ticked by default, as usual.

The page uses the management key from your CPA login. If it finds none, or
CPA does not accept it, the page asks for the key. A typed key stays in the
page only and is never saved.

If the config changes after the list was made, the save is refused and
nothing is written. The page then loads the current list.

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
  downloaded again when the page opens. **Update data** on the page downloads
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

The release build is `target/release/fill_model_details.dll`. On Windows the
C runtime is built into the file (see `.cargo/config.toml`), so it needs no
Microsoft Visual C++ runtime.

To make the zip for a release, run:

```sh
uv run --no-project python -I tools/make_release.py
```

It builds the DLL again without the folder names of your machine inside, adds
the licences of the libraries in the DLL, and writes the zip to `target/dist`.

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

The page's icons are from [Lucide](https://lucide.dev) (ISC License). The
licence text is in `third-party-licenses/lucide.txt`.

The release zip also holds the licences of the Rust libraries that are built
into the DLL, in its `third-party-licenses` folder.
