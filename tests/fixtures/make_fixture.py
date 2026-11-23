"""Cut the small test fixture out of a downloaded models.dev api.json.

The models.dev data is MIT licensed. Run:

    uv run --no-project python -I make_fixture.py <api.json> models-dev-sample.json
"""

import json
import sys

WANTED_MODELS = {
    "nvidia": [
        "z-ai/glm-5.3-flash",
        "minimaxai/minimax-m3.1-flash",
        "stepfun-ai/step-5-preview",
        "moonshotai/kimi-k3",
        "google/gemma-3-12b-it",
    ],
    "openrouter": [
        "moonshotai/kimi-k3.1-pro",
        "nvidia/nemotron-3.5-super-vl:free",
        "openrouter/free",
        "z-ai/glm-5.3-flash",
        "baidu/ernie-5.5-flash-preview:free",
    ],
    "moonshotai": ["kimi-k3.1-pro"],
    # Model makers and a reseller, for the canonical_model_id lookup.
    "zhipuai": ["glm-5.3-flash"],
    "openai": ["gpt-5.5"],
    "302ai": ["gpt-5.5"],
}


def main():
    source_path, target_path = sys.argv[1], sys.argv[2]
    with open(source_path, encoding="utf-8") as source_file:
        catalog = json.load(source_file)

    fixture = {}
    for provider_id, model_ids in WANTED_MODELS.items():
        provider = catalog[provider_id]
        kept_provider = {key: value for key, value in provider.items() if key != "models"}
        provider_models = provider["models"]
        kept_provider["models"] = {
            model_id: provider_models[model_id] for model_id in model_ids if model_id in provider_models
        }
        missing_ids = [model_id for model_id in model_ids if model_id not in provider_models]
        if missing_ids:
            print(f"{provider_id}: not in models.dev: {missing_ids}")
        fixture[provider_id] = kept_provider

    with open(target_path, "w", encoding="utf-8", newline="\n") as target_file:
        json.dump(fixture, target_file, indent=2, ensure_ascii=True)
        target_file.write("\n")
    print("models per provider:", {key: len(value["models"]) for key, value in fixture.items()})


if __name__ == "__main__":
    main()
