# Rust版 LiteLLM SDK 設計

作成日: 2026-10-25

実装状況: 以下のロードマップ各段階を単一 Crate に実装。Crate のバージョンは初期開発版 `0.1.0`。API の実例・制約・feature は [README](../README.md) を参照。CI は実 API を呼ばず、合成 Fixture とモック HTTP を使用する。

## 1. 概要

本プロジェクトは、LiteLLM の使いやすさを維持しながら、Rust の型安全性と拡張性を備えた LLM SDK を提供する。

中心的な特徴は次の通りである。

- `provider/model` 形式のモデル指定
- 型安全な統一 Request / Response
- 各プロバイダーのネイティブ API への接続
- Streaming、Tool Calling、Retry、Fallback の標準サポート
- 必要最小限の依存構成
- プロバイダー独自機能を失わない拡張性

## 2. スコープ

### 2.1 作るもの

LiteLLM 風の公開 API を持ち、プロバイダーごとの Adapter を内部で切り替える SDK を作る。

初期の中心機能は次の通りである。

- Chat API
- Streaming
- Tool Calling
- Retry
- Fallback
- Capability Validation

### 2.2 作らないもの

以下は初期スコープから除外する。

- Agent フレームワーク
- RAG
- Vector Store
- HTTP Proxy
- 管理画面

ライブラリの責務は「LLM API を安全かつ一貫したインターフェースで呼び出す」までに限定する。Router と Cache は利用者が明示的に設定する追加機能とし、直接呼び出しの利用に必須としない。Router はプロセス内の重み付きラウンドロビン、Retry/Fallback、障害接続先の一時除外を提供する。Cache は既定で無効にする。

## 3. 競合との差別化

2026年10月時点で、Rust にはすでに複数の LLM 用ライブラリが存在する。

| ライブラリ | 特徴 |
| --- | --- |
| genai | 25以上のプロバイダーと統一 API、ネイティブプロトコルをサポート |
| Rig | LLM アプリ、Agent、Tools、RAG などを扱うフレームワーク |
| async-openai | OpenAI 向けの型付きクライアント |

単に「OpenAI・Anthropic・Gemini を同じ API で呼べる」という機能だけでは差別化が弱い。本プロジェクトは次の三点を重視する。

1. **Provider Agnostic**: LiteLLM 風の `provider/model` 指定を維持する。
2. **Rust Native**: Rust の型、エラー、非同期設計に自然に沿う。
3. **Minimal Core**: Agent や RAG を含めず、LLM API 呼び出しのための最小コアを安定させる。

## 4. アーキテクチャ

公開 API は LiteLLM 風に保ち、内部のデータモデルは汎用的な Canonical Data Model とする。

```text
公開 API
  Client / ChatRequest / ChatResponse

Canonical Data Model
  Message / ContentBlock / ToolCall / Usage

Provider Registry + Capability Validation
  OpenAI Adapter
  Anthropic Adapter
  Gemini Adapter
  OpenAI-compatible Adapter

Shared HTTP Transport
  reqwest / tokio / TLS / Timeout
```

各 Adapter は、共通データモデルとプロバイダー固有 JSON、およびストリーミングイベントを相互変換する。これにより、OpenAI Responses API や Gemini Interactions API に新しいフィールドが追加されても、公開インターフェース全体を大きく変更せずに対応できる。

重要な原則は、共通化できない情報を捨てずに保持することである。Gemini 固有の署名など、履歴の再送に必要な情報は ContentBlock の Metadata に残す。応答の安全性情報などは ChatResponse.provider_metadata に読み取り専用で保持し、次のターンへ自動転送しない。

## 5. 公開 API

### 5.1 利用イメージ

```rust
use llm_unify::{Client, ChatRequest};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Client::from_env()?;

    let response = client
        .chat(
            ChatRequest::new("anthropic/claude-sonnet-5")
                .system("You are a helpful assistant.")
                .user("Rustの魅力を教えて")
                .max_output_tokens(1024),
        )
        .await?;

    println!("{}", response.output_text());

    Ok(())
}
```

モデルを切り替える場合は、基本的にモデル指定を書き換えるだけでよい。

```text
openai/gpt-6-astra
anthropic/claude-sonnet-5
gemini/gemini-3.8-flash
ollama/my-local-model
```

ただし、モデルが対応していないオプションを互換機能と見なしてはならない。対応しない機能は明示的にエラーにする。

## 6. Rust の型設計

### 6.1 ChatRequest

```rust
pub struct ChatRequest {
    pub model: ModelId,
    pub messages: Vec<Message>,
    pub options: GenerationOptions,
    pub tools: Vec<ToolDefinition>,
}
```

### 6.2 ContentBlock

```rust
pub enum ContentBlock {
    Text { text: String, metadata: Vec<ProviderMetadata> },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Image(ImageContent),
    Provider(ProviderMetadata),
}
```

### 6.3 ChatResponse

```rust
pub struct ChatResponse {
    pub model: ModelId,
    pub content: Vec<ContentBlock>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: FinishReason,
    // 実装では deployment、cache_hit、cost_usd、request_id も保持する。
}
```

この設計の狙いは次の三点である。

1. Message の内容を文字列に限定しない。Tool Calling や画像入力を自然に表現できる。
2. プロバイダー固有情報を保持する。Gemini の署名や他プロバイダーの特殊フィールドを次のターンに渡せる。
3. 将来拡張を考慮する。enum には `#[non_exhaustive]` を付与し、互換性のある形で種類を追加できる。

`ProviderMetadata` は単なる汎用 JSON ではなく、送信元プロバイダーの識別子を持たせる。これにより、異なるプロバイダーへの誤送信を防止する。

## 7. Provider Trait

公開 API とは別に、Provider Adapter 実装のための Trait を定義する。

```rust
#[async_trait]
pub trait Provider {
    async fn chat(
        &self,
        request: &ChatRequest,
    ) -> Result<ChatResponse, Error>;

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> Result<ChatStream, Error>;
}
```

初期実装では `async_trait` を使う。これは動的ディスパッチしやすくするためである。将来的に静的ディスパッチが必要になった場合には、内部実装を最適化できる。

## 8. Capabilities

各モデルの対応状況をメタデータとして扱う。

| Capability | 内容 |
| --- | --- |
| Streaming | ストリーミング応答に対応しているか |
| Tool Calling | Tool Calling に対応しているか |
| Image Input | 画像入力に対応しているか |
| Structured Output | JSON Schema 等による構造化出力に対応しているか |
| Reasoning Options | 推論系オプションに対応しているか |

`temperature` や `max_output_tokens` などのパラメーターも、モデルごとの対応状況を検証する。

未知のモデルについては、サポート有無を断定しない。明示的に提供された Capabilities を優先し、オプションを勝手に破棄しない。

## 9. Streaming 設計

Streaming は本 SDK の重要機能として設計する。プロバイダー固有のイベントを、共通の `ChatEvent` に変換する。

```rust
pub enum ChatEvent {
    TextDelta { index: usize, delta: String },
    ToolCallStart { index: usize, id: String, name: String },
    ToolCallDelta { index: usize, delta: String },
    Metadata { index: usize, metadata: ProviderMetadata },
    ResponseMetadata(ProviderMetadata),
    Usage(TokenUsage),
    Completed { reason: FinishReason },
}
```

実装では次の二つを分離する。

```text
Stream Decoder
  プロバイダー固有イベントを ChatEvent へ変換する

Stream Accumulator
  イベントを蓄積し、最終的な ChatResponse を組み立てる
```

Tool Calling の引数は途中で不完全な JSON になり得る。そのため、delta 単位で完全な JSON としてパースしない。

ストリームの途中切断は成功として扱わない。呼び出し側には、部分的な出力が存在することを伝えるエラーを返す。

## 10. エラー処理と信頼性

| エラー | 扱い |
| --- | --- |
| InvalidRequest | 共通型またはプロバイダー制約として不正なリクエスト |
| UnsupportedOption | モデルまたはプロバイダーが対応していない機能 |
| AuthError | API キー不足、無効な認証情報 |
| RateLimitError | レート制限による失敗 |
| ProviderError | プロバイダーが返した実行時エラー |
| TransportError | 通信障害、タイムアウト |
| StreamInterrupted | ストリームの途中終了 |

Retry は共通 HTTP 層に近い場所で扱う。対象は原則として次の通りである。

- HTTP 429
- 限定的な HTTP 5xx
- 再試行可能な通信障害

ただし、ストリーム出力を利用者へ一度でも返した後は、透過的な自動 Retry を行わない。重複出力や Tool Call の二重実行を防ぐためである。

Client は内部で HTTP 接続を共有し、`Clone + Send + Sync` を基本とする。API キーは Client ごとの設定に閉じ込め、ログに出力しない。

## 11. ディレクトリ構成

初期は単一 Crate 内のモジュールで開始する。

```text
llm-unify/
  Cargo.toml / Cargo.lock
  config/          # defaults.toml、capabilities.toml、pricing.toml、example.toml
  src/
    lib.rs / client.rs / config.rs / error.rs
    types.rs / registry.rs / capabilities.rs / stream.rs
    router.rs / cache.rs / embeddings.rs / observability.rs
    transport.rs
    providers/     # openai、anthropic、gemini、bedrock、vertex
  tests/
    fixtures/      # 合成 Fixture と出典
    contract/      # 共通契約・モック通信
  examples/
  .github/workflows/ci.yml
```

## 12. ロードマップ

### 第1段階（当初の v0.1）

| 項目 | 内容 |
| --- | --- |
| Core | Client、共通メッセージ型 |
| Provider | OpenAI Chat / Responses |
| Streaming | テキスト Streaming |
| Error | エラー変換 |

### 第2段階（当初の v0.2）

| 項目 | 内容 |
| --- | --- |
| Provider | Anthropic Messages、Gemini Interactions API |
| Tool Calling | Tool Calling と Tool Result の往復 |
| Metadata | プロバイダー固有メタデータの保持 |
| Structured Output | JSON Schema による構造化出力 |

### 第3段階（当初の v0.3）

| 項目 | 内容 |
| --- | --- |
| Router | Retry / Fallback、重み付きラウンドロビン、障害管理 |
| Alias | モデルエイリアス |
| Embeddings | Embeddings API |
| Observability | Token Usage / Cost、OpenTelemetry 連携 |

### 第4段階（当初の v0.4 以降）

| 項目 | 内容 |
| --- | --- |
| Multimodal | 画像などのマルチモーダル入力 |
| Provider | optional feature の Bedrock Converse / Vertex Gemini |
| Plugin | Rust Provider Trait の登録 API（動的ロードは対象外） |
| Cache | Cache Trait と TTL・容量制限付きメモリー Cache。Tool Call 応答も対象とし cache_hit を通知 |

## 13. テスト方針

各段階で Provider ごとの公開仕様に基づく合成 Fixture と共通契約テストを実行する。実レスポンスを追加する際は秘密情報を除去し、取得日・API バージョン・出典を記録する。クラウド接続は偽の認証情報とローカル HTTP により検証する。

特に次の項目を重視する。

1. Tool Call の往復
2. ストリーム途中切断
3. HTTP 429 Retry
4. 未対応オプションの明示的エラー
5. 機密情報のログ混入防止

## 14. 設定と運用

- Rust 1.96 / Edition 2024、Tokio の非同期 API を対象とする。
- 同梱設定は `config/`、利用者設定は `LLM_UNIFY_CONFIG` で指定する部分 TOML。Builder、環境変数、利用者設定、既定値の順に優先する。
- API キーは環境変数または ClientBuilder で渡す。`.env` は自動読込しない。
- 会話は呼び出し側で保持し、Responses / Interactions には `store: false` を送る。
- 既知の非対応機能は事前拒否し、未知モデルではオプションを保持する。
- ProviderMetadata は Provider と API 形式を持ち、異なる送信先への転送を拒否する。
- Retry と Fallback は共通の試行・時間上限を持つ。利用者への Streaming イベント送出後は再試行しない。
- Cache は非 Streaming の成功応答を対象にする。Client Clone 間で共有し、別々に構築した Client 間ではキー空間を隔離する。
- 料金表・モデル対応表の推測や自動取得は行わない。料金がない場合は `None`、料金がある場合は通常単価の概算を返す。
- Bedrock、Vertex、OpenTelemetry の依存は feature で選択する。外部 API への実疎通、crates.io 公開は自動実行しない。
