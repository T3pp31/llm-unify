# Changelog

## Unreleased

- LiteLLM 1.108.0の固定ソース、上流Rust/UI、改変履歴とハッシュ検証を追加。依存の競合は記録済みパッチで解消。
- 機能調査表の自動生成と、未分類・未実装・比較漏れ・実API未実施・負荷試験不足を拒否するリリース判定を追加。
- `llm-unify-engine` / `llm-unify-proxy` をWorkspaceへ追加。OpenAI ChatのHTTP/SSE、Tool履歴、Usage、429の比較試験を追加。LiteLLM OSS全機能の再現は未完了。
- `llm-unify-storage` にPostgreSQLの試行別台帳・複数予算の原子的予約/精算と、Redisの共有レート制限・テナント別Cacheを追加。実DBの並行・強制停止/復旧試験を追加。Proxy課金経路への接続は未完了。
- 比較結果のケース名・suite・接続先と現ソースの照合、部分成果の証跡登録、参照版の実行環境記録を追加。部分試験から全体の受け入れ成功へ昇格させない。

- Streaming 開始失敗と Chat / Embeddings の総時間上限によるタイムアウトを障害回数へ反映し、接続先の一時除外・復帰を修正。
- Client 経由の Streaming 最終応答に料金概算と、取得可能な上流 HTTP リクエスト ID を保持。
- Gateway の失敗応答にも台帳と同じ `x-request-id` を付与し、HTTP・通信・SSE エラーの照合を検証。
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
