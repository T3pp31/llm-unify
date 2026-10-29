# Pinned LiteLLM reference source

`litellm/` is imported from the archive specified in `config/parity/baseline.json`.
The upstream code and administration UI are MIT licensed; copyright notices are preserved in `litellm/LICENSE`.
`enterprise/` is excluded. Licenses for dependencies bundled with the UI are preserved unchanged.

Import with `python3 tools/parity/source.py import`; verify every file’s SHA-256 with `verify`.
The original directory layout preserves Rust relative references and upstream comparison test data.
`litellm/SOURCE.json` records the original hash of every imported file.
Upstream changes are recorded in `config/parity/patches.json` with a reason, before/after SHA-256 hashes, and unique replacement text,
then applied during import. `verify` rejects unrecorded direct edits.
Hashes in `SOURCE.json` continue to identify the original archive contents.

The dependency patches relax `aws-smithy-eventstream =0.61.4` to `^0.61.4` in the Rust framer and LLM test dependencies.
This permits resolution with 0.61.6 required by the existing SDK and prevents an unintended Bedrock SDK downgrade.

Test-data patches remove Japanese text, including escaped literals and compatibility symbols.
English examples and non-Japanese Unicode samples retain UTF-8 boundary and character-category coverage.
Tokenizer fixtures are regenerated with tiktoken 0.12.0 and the pinned Python reference's admission counter;
Python format fixtures are regenerated with CPython 3.14.5.
The tokenizer generator uses the pinned reference's `count_input_tokens_for_model` entry point.

Patches may use `edits`, an ordered list of `start`, `end`, and `after` fields.
Offsets are zero-based byte positions in the original, hash-verified file; `end` is exclusive.
Ranges must be ordered, non-overlapping, and within the original file. They are applied in reverse order
before any legacy `replacements`, then the result is checked against `patched_sha256`.
This records changes without copying removed test text into the patch manifest.
