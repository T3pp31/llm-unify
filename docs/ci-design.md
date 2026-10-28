# CI 設計

更新日: 2026-10-25

## 目的とトリガー

既存の SDK 契約を壊さないことを、実 API・秘密情報・課金なしで検証する。
実体は [ci.yml](../.github/workflows/ci.yml) に定義する。

- Pull Request と main への Push で実行する。
- `src/**`、`tests/**`、`examples/**`、`config/**`、Cargo 関連ファイル、workflow 自身の変更を検出する。
- `docs/**` や README のみの変更は対象外とする。
- `workflow_dispatch` では macOS・Windows の確認も実行する。

## 検証ジョブ

通常の test ジョブは Ubuntu / Rust stable で以下を実行する。

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features --all-targets
cargo test --locked --doc --all-features
```

`features` ジョブは MSRV の Rust 1.96.0 を使用し、追加 feature なし、bedrock のみ、vertex のみ、opentelemetry のみ、gateway のみを `cargo check --locked --no-default-features --features ... --all-targets` で検証する。all-features の検証と組み合わせ、feature 分離のコンパイル漏れを検出する。

手動実行の portability ジョブは macOS・Windows の stable で全 feature・全 target のテストを実行する。

## テスト対象

- Provider ごとの送受信変換、Tool Call / Result、署名・固有情報の保持。
- テキスト Streaming、Tool 引数の断片、UTF-8・SSE の分割、途中切断。
- 429 Retry、試行上限、Fallback、重み付き負荷分散、障害状態と復帰。
- Capabilities、設定、認証不足、機密情報の非露出。
- Cache の TTL・容量・Client 間の隔離・Tool Call 応答の再利用。
- Bedrock の AWS 署名とバイナリ EventStream、Vertex の認証ヘッダーと会話履歴。
- Gateway の HTTP/SSE 中継、認証、SQLite 永続化、料金表の保存、日別・モデル別集計、使用量不明・上流障害・途中切断・保存障害。

Fixture は公式資料に基づく合成データで、出典は [Fixture README](../tests/fixtures/README.md) に記録する。実サービスの疎通は examples を使って明示実行し、通常 CI には追加しない。

## 権限とキャッシュ

workflow の権限は `contents: read` のみ。シークレットは使用しない。
Cargo registry・Git dependencies・target をキャッシュし、OS・toolchain/feature・Cargo.lock のハッシュでキーを分離する。依存関係の解決は Cargo.lock をコミットし、`--locked` で固定する。

## 失敗時

失敗したステップとテスト名を確認し、同じコマンドでローカル再現する。
外部 Provider の API 仕様変更が疑われる場合は公式資料を確認し、確認日を更新して Fixture と Adapter を修正する。

## Gateway のローカル検証

```bash
cargo test --locked --features gateway --test gateway
cargo test --locked --all-features --all-targets
cargo test --locked --doc --all-features
```

Gateway のテストはローカルのモック上流と一時 SQLite を使用し、再接続時の永続性、料金表の変更、不明値、HTTP/SSE 障害、クライアント切断、同時実行上限、保存障害を検証する。実際の Ollama の起動・推論は含まない。

カバレッジ計測は現状の CI に未導入。必要な環境に `cargo-llvm-cov` と toolchain に対応する `llvm-tools-preview` を導入した後、`cargo llvm-cov --all-features --html` で HTML レポートを取得できる。今回の検証ではカバレッジ率を計測していない。
