# Changelog

## Unreleased

- optional `gateway` feature と `llm-unify-gateway` バイナリを追加。OpenAI Chat Completions 互換の HTTP/SSE 中継、Bearer 認証、SQLite の使用量台帳、料金表の保存、日別・モデル別集計に対応。
- Gateway の使用量不明・料金未設定・途中切断・保存障害の検証を追加し、CI に gateway 単独構成を追加。

- Client、共通 Request / Response、Capabilities、ProviderMetadata を追加。
- OpenAI、Anthropic、Gemini、OpenAI-compatible、Ollama のネイティブ接続を追加。
- Streaming、Tool Calling、Structured Output、Reasoning Options、画像入力を追加。
- Retry/Fallback、Alias、重み付き負荷分散、障害接続先の一時除外を追加。
- Embeddings、任意の料金計算、Observer／OpenTelemetry を追加。
- optional feature として Bedrock Converse、Vertex Gemini を追加。
- 公開 Provider / Cache Trait と TTL・容量制限付きメモリー Cache を追加。
- 実サービスを呼ばない契約テストと CI を追加。
