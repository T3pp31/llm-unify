# Third-party notices

LLM Unify is distributed under the MIT License; see [LICENSE](LICENSE).

The Rust sources in `vendor/litellm/litellm-rust/` originate from
[BerriAI/LiteLLM](https://github.com/BerriAI/litellm), version 1.108.0,
commit `dddbaa803d6e9fcc862ea7a5412f771cb1ae040e`, under the MIT License.
Their original copyright and permission notice is preserved in
[vendor/litellm/LICENSE](vendor/litellm/LICENSE). Modifications and their
before/after hashes are recorded in `config/parity/patches.json`.
`vendor/litellm/SOURCE.json` records the retained upstream files.

The Python LiteLLM server and its web dashboard are not part of this distribution.
Optional comparison-test inputs live outside the tracked distribution.
The LLM Unify interface in `crates/proxy/ui/` is part of this project.

Other dependencies retain their respective licenses, as specified in their
Cargo package metadata and source distributions. `Cargo.lock` fixes their
versions for reproducible builds.
