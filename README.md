# llm-unify

`provider/model` でネイティブ API を切り替える、サーバー・CLI 向けの非同期 Rust SDK です。
Rust 1.96 以降、Edition 2024、Tokio を使用します。Python LiteLLM 全体の互換実装ではありません。optional な `gateway` feature は OpenAI Chat Completions 互換の HTTP 中継と使用量台帳を提供します。

## 利用例

```rust,no_run
use llm_unify::{ChatRequest, Client};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let client = Client::from_env()?;
let model = std::env::var("LLM_UNIFY_MODEL")?;
let response = client.chat(ChatRequest::new(model)
    .system("You are a helpful assistant.")
    .user("Rust の魅力を教えてください。")
    .max_output_tokens(1024)).await?;
println!("{}", response.output_text());
# Ok(()) }
```

リポジトリ内の例は以下で実行できます。使用する Provider の認証情報だけを設定してください。

```bash
export OPENAI_API_KEY='your-key'
export LLM_UNIFY_MODEL='openai/your-model'
cargo run --example chat
cargo run --example streaming
cargo run --example tools
```

SDK は Tool を実行しません。`ToolCall` をアプリケーションで処理し、`response.into_message()` と `Message::tool(ToolResult)` を次のリクエストに渡してください。複数 Tool Call には全件の Result が必要です。

## 対応 API

| Provider | API | Chat / Stream / Tools / 画像入力 | Feature |
|---|---|---|---|
| `openai` | Responses（既定）、Chat Completions | 対応 | 標準 |
| `anthropic` | Messages | 対応 | 標準 |
| `gemini` | Interactions、steps 形式 | 対応 | 標準 |
| `ollama`、利用者定義の互換接続先 | OpenAI Chat Completions 互換 | 接続先の対応機能に従う | 標準 |
| `bedrock` | Converse / ConverseStream | 画像はバイト列のみ | `bedrock` |
| `vertex` | Vertex Gemini generateContent / streamGenerateContent | 対応 | `vertex` |

Structured Output と Reasoning Options は各ネイティブ API に変換します。モデル自体の対応状況は別途確認してください。
Embeddings は OpenAI と Embeddings 対応の OpenAI-compatible 接続先に対応します。
音声・動画・画像生成、Bedrock InvokeModel、Vertex 上の他社モデルは対象外です。

Cloud と OpenTelemetry は optional feature です。Bedrock は AWS SDK の認証チェーン、Vertex は Google ADC を使用し、認証情報とトークン更新は公式ライブラリに委ねます。

## 設定

既定値は `config/defaults.toml`、既知モデルの対応機能は `config/capabilities.toml`、任意の料金表は `config/pricing.toml` をビルド時に同梱します。モデルの対応機能・料金は現在空で、推測値を含めていません。

設定優先順位は Builder の明示設定、環境変数、利用者 TOML、同梱既定値の順です。`Client::builder()` は同梱既定値から開始し、`Client::from_env()` は `LLM_UNIFY_CONFIG` と設定用環境変数も読み込みます。

```bash
cp config/example.toml config/local.toml
export LLM_UNIFY_CONFIG=config/local.toml
```

TOML は部分指定できます。API キーの値は TOML に記載せず、`api_key_env` に環境変数名を設定するか、Builder の `api_key()` で渡してください。`.env` は自動読込しません。

Provider ごとに `LLM_UNIFY_<接続先名の大文字>_BASE_URL`、`_API`、`_API_VERSION`、`_PROJECT`、`_LOCATION`、`_REGION` で TOML を上書きできます。接続先名中の `-` は `_` に変換します。
Vertex は `project`・`location`、Bedrock は設定または AWS 認証チェーン側の region が必要です。

`capabilities."provider/model"` で明示した対応機能を優先します。`false` は送信前に拒否し、未指定は未知としてオプションを維持して送信します。未知のモデルを名前の類似で判定しません。
`ProviderMetadata` は Provider と API 形式の両方を識別し、別 Provider／別 API への転送を拒否します。

履歴に必要な署名・メッセージ ID・`phase` は ContentBlock の Metadata に保持し、`into_message()` で再送できます。応答の安全性情報や生の Usage 等は `response.provider_metadata` に読み取り専用で保持し、履歴へ自動転送しません。`response.model` は API がモデル名を返す場合にはその実モデル名を示します。

会話履歴は呼び出し側で保持します。Responses と Interactions には `store: false` を送信します。これはサーバー側会話状態の指定であり、サービス全体のデータ保持方針を変更するものではありません。

## Router・Retry・Streaming

`config/example.toml` に Alias と Route の設定例があります。`alias/chat` のように指定すると、接続先グループを重み付きラウンドロビンで選びます。同じ Client の Clone は負荷分散・障害状態を共有します。

- 429、設定された 5xx、一部通信障害を再試行します。`Retry-After` を尊重し、それ以外は上限付き指数バックオフとジッターを使用します。
- 全接続先を通算した試行上限と総時間上限を適用します。入力不正と認証エラーは自動再試行しません。
- 接続先の連続障害で一時除外し、待機期間後に一つの試行で復帰を確認します。全接続先が除外中なら即座にエラーになります。
- 初期選択した接続先が失敗した場合は、グループ内の残り、明示 Fallback の順に試します。機能や Metadata が不適合な候補には送信しません。
- イベントを一つでも利用者に返した後は、Streaming の Retry／Fallback を禁止します。

`ChatStream::collect_response()` で最終応答を組み立てられます。正常な終了イベントがない切断は `StreamInterrupted` となり、`error.partial` に生成済み内容と未完成 Tool 引数を保持します。Stream を破棄すると通信を中止します。

Gemini Interactions の `incomplete` は API が正常終了を通知した状態として `FinishReason::Unknown("incomplete")` に保持します。終了原因を API が返していない場合に、トークン上限と断定しません。

## Cache・拡張・観測

Cache は既定で無効です。有効化すると非 Streaming の成功応答を TTL・容量制限付きで保持します。**Tool Call を含む応答も保存します**。`response.cache_hit` を確認し、Tool の再実行が適切かをアプリケーションで判断してください。Tool Call ID を再発行する処理は行いません。

Clone 間では共有し、別々に構築した Client 間では、同じ Cache を登録してもキー空間を隔離します。外部 Cache のエラーは成功した API 応答を失敗に変えません。Streaming と Embeddings は Cache 対象外です。

`Provider` と `Cache` は公開 async Trait です。`Client::builder().provider(name, Arc::new(...))`／`.cache(...)` で独自実装を登録できます。動的ライブラリロードや Redis 実装は含みません。

`Observer` にはモデル、接続先、時間、試行回数、Usage、Cache 状態、エラー種別を渡します。`opentelemetry` feature はアプリケーションが設定したグローバル Meter／Tracer を使用します。Exporter の作成や本文記録は行いません。

料金表がある場合のみ `cost_usd` を計算します。これは設定した通常単価による概算であり、キャッシュ割引・バッチ割引等は含みません。料金表がない場合は `None` です。

## 検証

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features --all-targets
cargo test --locked --doc --all-features
cargo check --locked --no-default-features --all-targets
```

通常テストはモック HTTP と合成 Fixture のみを使用し、API キー・クラウド認証情報・課金を必要としません。実サービスへの疎通は `examples` を利用して明示実行してください。Fixture の出典と確認日は `tests/fixtures/README.md` に記載しています。
Linux の CI に加え、手動の workflow_dispatch で macOS・Windows の全 feature テストを実行できます。crates.io への公開は行っていません。

## ローカル LLM の中継と使用量台帳

`gateway` feature は、Ollama 等の OpenAI 互換サーバーに対して通常応答と SSE Streaming を中継します。`openai_compatible` 接続先、または `api = "chat_completions"` を設定した `openai` 接続先を使用できます。API は `POST /v1/chat/completions` です。モデル名は SDK と同じ `provider/model` を受け取り、上流には最初の `/` より後の部分を送ります。その他のリクエストフィールドと応答 JSON は維持します。

```bash
cp config/example.toml config/local.toml
export LLM_UNIFY_CONFIG=config/local.toml
export LLM_UNIFY_GATEWAY_API_KEY='replace-with-a-long-random-secret'
cargo run --features gateway --bin llm-unify-gateway
```

`config/local.toml` の `[gateway]` に待受アドレス、SQLite パス、認証用環境変数名、リクエストサイズ・同時実行・履歴取得件数の上限、SQLite 待機時間を設定します。パスは起動ディレクトリからの相対指定または絶対指定です。DB の親ディレクトリとスキーマは起動時に作成します。設定例の待受はループバックです。外部公開する場合の TLS はリバースプロキシ等で設定してください。

Ollama が起動し、対象モデルがインストール済みであることを確認してから呼び出してください。

```bash
curl http://127.0.0.1:4000/v1/chat/completions \
  -H "Authorization: Bearer $LLM_UNIFY_GATEWAY_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"model":"ollama/your-installed-model","messages":[{"role":"user","content":"こんにちは"}]}'

curl http://127.0.0.1:4000/usage/requests \
  -H "Authorization: Bearer $LLM_UNIFY_GATEWAY_API_KEY"
curl http://127.0.0.1:4000/usage/summary \
  -H "Authorization: Bearer $LLM_UNIFY_GATEWAY_API_KEY"
```

全 API に同じ Bearer 認証が必要です。上流の API キーは既存の Provider 設定の `api_key_env` から読み込み、呼び出し元の認証ヘッダーは上流へ転送しません。プロンプト・生成本文・API キーは DB に保存しません。モデル名、時刻、状態、トークン数、料金表、料金概算を保存します。`x-request-id` で応答と台帳を照合できます。

- 使用量は上流の `usage` を取得します。独自 tokenizer による推定は行いません。不明な値は `null`、実測ゼロは `0` です。
- Streaming では `stream_options.include_usage = true` を送信します。上流がこれを受け付け、使用量を返す必要があります。使用量イベントはクライアントにも返します。`[DONE]` がない切断は失敗として記録し、SSE エラーを返します。
- `[prices."provider/model"]` がある場合のみ、通常の入力・出力単価で `estimated_cost_usd` を計算します。記録時点の料金表・出典・適用日を各行に保存し、設定変更で過去の料金を書き換えません。ローカル運用原価を扱う場合は、その想定単価を利用者が設定してください。実際の API 請求額や GPU・電力使用量は計測しません。キャッシュ割引・段階料金等も未対応です。
- 中継は自動 Retry・Fallback・Cache を使用しません。受理した1件につき上流送信は最大1回です。クライアントが再送した場合は別リクエストとして記録します。
- 上流への送信前に `pending` 行を永続化し、応答前に `completed`／`failed` に更新します。クライアント切断は `cancelled` に更新します。プロセス強制終了等で確定できなかった行は `pending` のまま残り、成功や無料とは扱いません。保存障害時には HTTP または SSE エラーを返します。
- `/usage/requests` は新しい順です。`limit` と、返却された `next_before` を `before` に指定してページを取得できます。両 API は `from_ms` 以上・`to_ms` 未満の Unix ミリ秒と `model` で絞り込めます。`/usage/summary` は UTC の日別・モデル別に集計し、取得上限を超えた場合は `truncated: true` を返します。期間・モデルを絞って再取得してください。不明件数を必ず併せて確認し、既知の料金の合計を全体の確定料金と解釈しないでください。

Anthropic／Gemini 等への HTTP 形式変換、利用者別キー管理、予算による停止、管理 UI はこの中継の対象外です。各ネイティブ API は既存 SDK から利用できます。

参照元は `example_code/litellm-main.zip`（コミット `dddbaa803d6e9fcc862ea7a5412f771cb1ae040e`、SHA-256 `b9f9058eb9141baaa7f072ca6be448f2e947d6b264a032a3579a613b63b45a99`）の `ARCHITECTURE.md`、`litellm/proxy/hooks/proxy_track_cost_callback.py`、`litellm/proxy/db/db_spend_update_writer.py`、`litellm-rust/crates/cost/src/lib.rs` です。通常部分の MIT ライセンスを確認し、責務分離・不明単価の扱い・送信ごとの記録を参考に独自実装しています。`enterprise/` のコードは使用していません。
