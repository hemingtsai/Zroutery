# Model Catalog Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-15 · Verified by: `cargo test --workspace --all-features --locked`

models.dev supplies the capabilities a provider's own `/models` route rarely
answers: context window, max output, vision/audio/video/file support, tools,
structured output, thinking and effort levels. Discovery uses it; the saved
configuration stays authoritative.

## Source and cache

- The source of record is `https://models.dev/api.json`
  (`crates/zroutery-core/src/models_dev.rs::CATALOG_URL`).
- The catalogue is large (the module's own estimate, not a measured size),
  fetched at most once per hour and cached in memory
  (`crates/zroutery-core/src/models_dev.rs::catalog`).
- `parse` builds the provider → model map; `parse_provider` takes one section
  (`crates/zroutery-core/src/models_dev.rs::parse`,
  `crates/zroutery-core/src/models_dev.rs::parse_provider`).
- `CatalogModel` is the mapped metadata
  (`crates/zroutery-core/src/models_dev.rs::CatalogModel`).

## What is mapped

`catalog_model` maps one entry (`crates/zroutery-core/src/models_dev.rs::catalog_model`):

| Catalogue field | Gateway field |
|---|---|
| `limit.context` | `context_window` |
| `limit.output` | `max_output` |
| `cost.input == 0 && cost.output == 0` | `free` |
| `modalities.input` contains `image`/`video`/`audio` | `capabilities.vision`/`video`/`audio` |
| `attachment` | `capabilities.files` |
| `tool_call` | `capabilities.tools` |
| `reasoning` | `capabilities.thinking` |
| `structured_output` | `capabilities.structured_output` |
| `reasoning_options` type `effort` | `effort_levels`, filtered to `EFFORT_LEVELS` |

A model that does not declare reasoning has no effort levels. A toggle-only
model has none either; only the effort options contribute
(`crates/zroutery-core/src/models_dev.rs::effort_levels`).

## Lookup

`Catalog::lookup(provider_id, kind_hint, model_id)` widens in a fixed order
(`crates/zroutery-core/src/models_dev.rs::Catalog::lookup`):

1. the provider's own id;
2. the id its kind implies (the OpenCode family is `opencode`);
3. the model id across every provider, after stripping a relay's `vendor/` or
   `zone:` prefix.

Cross-provider matches are conservative: a capability is claimed only when every
match claims it, a limit is published only when every match publishes one, and
effort levels are never taken from a cross-provider match. `auto` is refused
cross-provider because each relay's auto pool is a different service.

## Enrichment

`enrich` fetches and applies the catalogue; `enrich_from` applies an
already-fetched one (`crates/zroutery-core/src/models_dev.rs::enrich`,
`crates/zroutery-core/src/models_dev.rs::enrich_from`).

The provider's own `/models` response wins where it already spoke:

- prices are never replaced by models.dev;
- `free` is only raised, never lowered, because a provider that says a model is
  free is better evidence than a catalogue default;
- capabilities and effort levels are filled from the catalogue.

## Manual corrections win

The module only shapes the *discovery* result. The candidate values are copied
into a `ModelEntry` when the user imports a model; after that the saved
configuration is the authority. Re-fetching cannot overwrite an edited field,
because the import surface does not add a model the configuration already has;
the backend never merges into configured entries
(`ui/src/pages/models/CatalogueImport.tsx::CatalogueImport`,
`crates/zroutery-core/src/models_dev.rs::enrich_from`).

## Boundaries

- No price data from models.dev beyond the zero-cost flag.
- No cross-provider effort levels.
- No write to the configuration; discovery and the user do that.

## Sources

- `crates/zroutery-core/src/models_dev.rs::CATALOG_URL`
- `crates/zroutery-core/src/models_dev.rs::catalog`
- `crates/zroutery-core/src/models_dev.rs::parse`
- `crates/zroutery-core/src/models_dev.rs::parse_provider`
- `crates/zroutery-core/src/models_dev.rs::CatalogModel`
- `crates/zroutery-core/src/models_dev.rs::catalog_model`
- `crates/zroutery-core/src/models_dev.rs::effort_levels`
- `crates/zroutery-core/src/models_dev.rs::Catalog::lookup`
- `crates/zroutery-core/src/models_dev.rs::enrich`
- `crates/zroutery-core/src/models_dev.rs::enrich_from`
- `crates/zroutery-core/src/models_dev.rs::provider_catalog`
