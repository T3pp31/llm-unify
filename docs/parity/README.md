# LiteLLM OSS互換実装の検証記録

全機能の再現・製品版の受け入れは未完了です。現在のProxyを本番移行先として扱わないでください。
既存のRust SDK・TOML・`llm-unify-gateway`のSQLite台帳と、開発中の互換Proxyは別コマンドです。
既存Gatewayの「受理1件につき上流送信最大1回」の契約は維持します。

基準はLiteLLM **1.108.0**、コミット`dddbaa803d6e9fcc862ea7a5412f771cb1ae040e`です。
参照ZIP、SHA-256、抽出範囲、元ソースmanifestのSHA-256は`config/parity/baseline.json`に固定しています。
MITの著作権表示は`vendor/litellm/LICENSE`等に保存します。リポジトリには上流Rustとそのテスト資材だけを保持し、Python版本体・上流Web UIは同梱しません。
比較用の固定ソースはGit対象外の`target/reference/litellm/`へ配置します。Enterpriseディレクトリは取り込みません。
上流Rustの依存制約への変更は`config/parity/patches.json`に理由・変更内容・変更前後のハッシュを記録しています。

## 現在の範囲

| 項目 | 状態 |
|---|---|
| 上流Rustの再利用、ライセンス・ハッシュ、外部比較ソースの分離 | 実装済み。取り込みは機能実装の証拠に数えない |
| HTTP、Provider、Provider/API、設定、SDK公開名、Router、Cache、認証、管理、Guardrail、連携、UIの静的抽出 | 調査候補を生成済み。OSS/Enterprise判定・動的登録の精査・契約レビューは未完了 |
| OpenAI Chatの通常応答、Tool Call、Tool結果の往復、SSE、上流429 | 固定Python版と模擬上流を使う比較ケースを実装 |
| 上流RustのResponses、Messages、OCR、Transcription等 | 経路を接続済み。各機能のPython比較は未完了 |
| YAML | 対応済みキーだけ受理。未対応の予算・Router・Cache等を黙って無視しない |
| 既存SDKを共通実行基盤へ接続、全Provider/API、料金、Router、Cache backend | 未完了 |
| PostgreSQLの試行台帳・原子的複数予算予約/精算、Redis共有レート制限/Cache | Rust APIと実DB試験を実装。Proxyの全送信経路への接続は未完了 |
| 仮想キー、利用者/チーム/組織管理、料金表との接続、管理API | 未実装 |
| Webコンソール `/ui/` | Rust自身が配信。認証付き設定モデル一覧・稼働状態を表示。管理操作は未実装 |
| 管理画面の全操作・操作E2E、DB移行、バックアップ/復元 | 未実装 |
| 代表7接続先の実API、2台/100同時/1時間の障害・継続稼働試験 | 未実施。Codexのローカルルーターは別枠の実接続試験として扱う |

`inventory.json`は静的な調査候補の一覧です。件数を機能数・OSS対応数・実装済み数と読み替えません。
各候補の`scope`、入出力契約、実装状態、検証ケースはレビュー前の値で生成します。
確認済みの実装判断は`config/parity/implementation.json`へ別途保存し、自動抽出で上書きしません。
OpenAI Chat関連の記録は`partial`です。5ケースを通過しても、Provider/APIの全契約が揃うまでは`implemented`へ昇格しません。
関数・型・内部補助処理も候補に含むため、独立した製品機能かどうかの精査も必要です。

## 固定ソースと調査表

Python 3.12以上が必要です。上流PythonはimportせずASTを解析します。

```bash
python3 tools/parity/source.py verify
python3 tools/parity/workspace.py check
python3 tools/parity/inventory.py check
python3 -m unittest discover -s tests/parity -v
```

参照ZIPは`config/parity/baseline.json`の`archive`へ配置します。SHA-256が固定値と一致するものだけ使用します。
`python3 tools/parity/source.py import`はRust資材だけを取り込みます。Python版をvendorへ復元しません。
比較前に`python3 tools/parity/source.py import --reference`を実行すると、固定ソースをGit対象外の`target/reference/litellm/`へ展開します。
`source.py verify --reference`で全比較資材を検証できます。
通常の`inventory.py check`は固定調査表のハッシュと基準版を検証し、`inventory.py check --reference`は外部比較ソースから再生成して照合します。
全体リリース判定は外部比較ソースも要求し、未配置なら失敗します。
`SOURCE.json`は元ファイルのハッシュを保持し、記録済みパッチだけ適用・照合します。
調査表の再生成は`python3 tools/parity/inventory.py generate`です。

Cargoの入れ子Workspace継承問題（[Cargo #12154](https://github.com/rust-lang/cargo/issues/12154)）のため、
ルートCargo.tomlの生成セクションに上流のpackage/dependency定義を同じ指定で保持しています。
生成は`python3 tools/parity/workspace.py generate`、パス以外の差分がないかの確認は`check`です。
上流Rustのテスト全体を実行したものとは区別し、このプロジェクトのcrateは`-p`で指定します。

## Proxyの実行

```bash
cargo build --locked -p llm-unify-proxy
cargo run --locked -p llm-unify-proxy -- \
  --config /absolute/path/to/config.yaml --listen 127.0.0.1:4000 --check
cargo run --locked -p llm-unify-proxy -- \
  --config /absolute/path/to/config.yaml --listen 127.0.0.1:4000
```

受理するキーは`config/parity/native-capabilities.json`に記載しています。
`model_list`の`model_name`と`litellm_params`、`general_settings.master_key`、`environment_variables`を扱います。
接続先・キーには`os.environ/変数名`を指定できます。未設定・空のmaster keyは起動エラーです。
独自Python認証の設定は、Rustへの移行が必要であることを示して拒否します。
同じ`model_name`の複数deploymentはRouter実装前のため拒否し、1件に潰しません。

実行期限・上限・OpenAIの既定URLは`config/parity/native-defaults.json`をビルド時に読み込みます。
`--runtime-config /absolute/path/to/runtime.json`で同じスキーマの外部JSONに置き換えられます。
HTTP本文やAPIキーはログに記録しません。現在のProxyには永続使用量台帳がありません。
OpenAI Chatの再試行は既存Gatewayから独立しており、429と5xxに既定2回を上限に行います。
全Router戦略・Fallback・課金記録との整合性を実装したものではありません。

## Webコンソール

Proxyを起動して`http://<listenのアドレス>/ui/`へアクセスします。HTML・CSS・JavaScriptは
`llm-unify-proxy`バイナリに埋め込み、同じRustプロセスから配信します。Node.js・Pythonサーバーは不要です。
画面の接続欄にこのProxyのmaster keyを入力すると、`/ui/api/status`をBearer認証で取得します。
表示するのは公開モデル名、製品バージョン、接続用のAPI経路です。上流URL・API key・設定全体を返しません。
キーはページのメモリ内のみで使用し、接続解除・ページ離脱で破棄します。
UIレスポンスは`no-store`、同一originに限定したCSPを設定しています。
`config/proxy-ui.json`がAPI表示情報・画面通信期限の定義で、ビルド時に読み込みます。
配布・運用手順は`docs/operations.md`を参照してください。
この画面は管理APIの完成を意味しません。設定編集・仮想キー管理・使用量・請求額はまだ提供しません。

## 永続台帳と共有制限の基盤

`crates/storage`のRust APIを`llm_unify_engine::storage`から公開しています。
この公開はHTTP Proxyでの課金・予算・Cacheの有効化を意味しません。現在は基盤の単独試験までです。
既存GatewayのSQLite台帳はそのまま維持しています。

PostgreSQLを正本とし、キー・利用者・チーム・組織等に相当する複数の予算accountを
一つのトランザクションで予約・精算できます。accountの関連付けは呼び出し元が渡すため、
仮想キー認証と全適用予算の解決は別途実装が必要です。Redisを予算の正本にはしません。

試行状態は`prepared → dispatched → settled`です。送信結果が不明なら`uncertain`にできます。
`dispatch`を成功させた呼び出し元だけが上流送信を開始できます。再試行には別の試行IDとordinalが必要です。
送信前の`prepared`だけを`released`にできます。送信後は期限やProxy再起動だけで解放せず、
実費が確定するまで予約を保持します。応答切断・精算コミット結果不明は未確定として照合する設計です。
実費が予約額を超えた場合も実費を記録し、超過した予算は後続予約を拒否します。
金額・トークン数の不明値とゼロは区別し、ProviderのCache・Reasoning等のUsage詳細も元のJSONとして保存します。
現APIの金額はUSDの10億分の1単位の整数で、より細かな小数を黙って丸めません。
全Providerの料金計算・変換との互換性は未完了です。

`Ledger::connect`は環境変数のURLを使います。`Ledger::migrate`をサービス開始前に明示実行し、
新規の移行先DBへ`llm_unify`スキーマを作ります。これは内部スキーマの初期化であり、
LiteLLM PostgreSQLやSQLiteからのデータ移行コマンドではありません。
元DBへこのマイグレーションを実行しないでください。接続数・取得期限・SQL実行期限・ロック期限は
`PostgresSettings`で指定します。URLやDBからの秘密情報をエラー表示へ含めません。

Redisは全適用制限をLuaでまとめて判定し、一つでも不足すれば消費しません。
同じadmission IDと同じ入力は最長windowの間重複消費せず、違う入力は競合として拒否します。
windowはRedis側のTTLを使い、クライアント時計には依存しません。これは固定windowの消費制限です。
同時実行数のlease、TPMの実測精算、LiteLLM全Router制限を実装したものではありません。
テナントの認可scopeとリクエストfingerprintを必須にするCache APIも提供します。
Redis障害はエラーを返します。Redis Clusterでは一つのnamespaceを同一hash slotへ置いて原子性を保ちます。
再起動時に制限を維持するにはRedis永続化も必要で、試験ではAOFと`appendfsync always`を使用します。

単独のユニット試験：

```bash
cargo test --locked -p llm-unify-storage --lib
```

実DB・100並行・独立した2接続pool・PostgreSQL/Redis強制停止と復旧を含む試験：

```bash
python3 tools/parity/run_storage.py
```

Python 3.12以上と稼働中のDocker、`config/parity/storage-test.json`のimageが必要です。
imageを未取得の場合は同ファイルの`postgres_image`と`redis_image`を`docker pull`してください。
runnerは試験専用資格情報・DB・Redis・空きポートを生成し、自分で作ったcontainerとvolumeだけを削除します。
サービス停止時のエラー、復旧後の予約とレート制限の保持、精算再送時の重複なしを確認します。
設定としきい値は同JSONから読み込み、結果とimage IDは`target/parity/storage.json`、
詳細は`storage-*.log`に保存します。これは2台のHTTP Proxyや1時間の負荷試験の代わりにはなりません。
通常の`cargo test`では実DB試験をignoredにし、runnerが`--ignored`を指定して全件実行します。
未実施・0件・ignoredが残る実DB実行は成功レポートにしません。
カバレッジの採用ツールは未設定です。`cargo-llvm-cov`導入後は、同じ隔離DBの環境変数を設定して
`cargo llvm-cov -p llm-unify-storage --test postgres --test redis -- --ignored`で計測できます。

## Python比較試験

`config/parity/cases.json`に同じ入力と模擬上流応答を指定します。
比較対象はHTTP状態、JSONフィールド、順序を保持したSSEイベント、Usage、上流送信内容・回数です。
現時点のケースはChatの5件であり、料金・全API・全Providerを検証したものではありません。
除外はケースごとに記載した生成ID・時刻のJSON pointerだけを許可します。
不完全な環境やケース0件を成功とは記録しません。ログ・試験結果は`target/parity/`へ保存します。

固定依存のローカルPython環境を作る場合：

```bash
uv export --frozen --no-dev --extra proxy --no-emit-workspace \
  --project target/reference/litellm --output-file target/parity-requirements.txt --quiet
uv venv --python 3.12 target/parity-reference-venv
uv pip sync --python target/parity-reference-venv/bin/python \
  --require-hashes target/parity-requirements.txt
python3 tools/parity/run_local.py --python target/parity-reference-venv/bin/python
```

Python参照版だけをコンテナで実行し、同じRustバイナリと比較する場合：

```bash
python3 tools/parity/run_local.py \
  --reference-image python:3.12-slim --uv-version 0.11.17 --startup-seconds 180
```

このコマンドはローカルDockerイメージを使用し、内部で固定lockfileの依存をhash付きで同期します。
参照コンテナと模擬上流は試験終了時に停止します。外部サービスの実APIキーは使用しません。
native/reference/mock/compareをすべてコンテナで実行する構成は`deploy/parity.compose.yaml`です。
`config/parity/containers.env.example`と、実行時の`PARITY_MASTER_KEY`・`PARITY_SUBJECT`を使用します。
`PARITY_SUBJECT`は`python3 tools/parity/gate.py --subject`で取得し、master keyは試験専用の乱数を使います。
Composeの起動と停止は`docker compose --env-file config/parity/containers.env.example -f deploy/parity.compose.yaml up --build --abort-on-container-exit --exit-code-from compare`と、同じ指定で`down`です。
PostgreSQL/Redisを含む本番Composeではありません。全コンテナ構成のビルド・試験が未完了である限り受け入れを通しません。

## 完成判定と証跡

### CodexのローカルLLMを使う実接続試験

```bash
python3 tools/parity/run_live.py --codex-config /absolute/path/to/.codex/config.toml
```

アクティブなProviderの`base_url`とモデルを読み、Rust Proxyの`/v1/responses`経由で通常応答・
Streaming・強制Tool Call・Tool結果の往復を検証します。モデルを明示する場合は`--model`を指定します。
Codexの`auth.json`やOAuth tokenは読みません。Providerの`env_key`があればその環境変数を使い、
認証不要のloopbackルーターでは非secretのplaceholderを使います。非loopback接続にはHTTPSと明示API key環境変数を要求します。
追加認証headerやquery設定を使うProviderは黙って設定を無視せず拒否します。

入力・出力上限・時間制限は`config/parity/live-settings.json`で管理します。
結果は`target/parity/live-local-router.json`に保存し、要求モデルと応答の実モデル名、Usage、
Streaming順序と再構成、ToolのJSON引数とcall IDの往復を確認します。
API key、Authorization header、生成本文、Tool引数の実データ、Proxyの生ログはreportへ保存しません。
接続先が返すmodel名が要求aliasと異なる場合もそのまま記録します。モデルの実体・hardware配置を推定して認定しません。

ローカルルーターの結果は`local_router_live`という部分試験です。
OpenAI・Anthropic・Gemini・Azure・Bedrock・Vertex・Ollamaの各実API受け入れや、固定Python版との全項目比較を代替しません。
部分証跡には`evidence.py --component-report target/parity/live-local-router.json`を指定できます。
ソース変更後は他の証跡と同様、再実行が必要です。CLIは失敗・未完了応答・0件を合格にしません。

### リリース判定

```bash
python3 tools/parity/gate.py
```

結果は`target/parity/release.json`です。未実装の現状では終了コード1が正しい結果です。
固定版・調査表の一致に加え、全候補の対象分類、入出力契約、実装、ケースごとの証跡を要求します。
除外はEnterprise固有・Python SDK構文・独自Python直接実行だけで、理由と上流の根拠箇所が必要です。

実装レコードは候補IDをキーに次の形で記載します。

```json
{
  "scope": "oss",
  "status": "implemented",
  "contract": {"input": "レビュー済み入力契約", "output": "レビュー済み出力契約"},
  "verification_cases": ["ケースID"]
}
```

実行証跡のmanifestは`target/parity/evidence.json`へ保存します。`schema_version: 1`、
`baseline`（version/commit/archive_sha256）、`subject`（現ソースのSHA-256）、`cases`、`suites`、`live`、`load`を持ちます。
`cases`の各証跡は`status: "passed"`、検証した`features`のID配列、ローカル`report`の相対パス、ファイルの`sha256`を持ちます。
`suites`と`live`も同じreport/hash/status形式です。report本文にも同じ`subject`、`status`、
正の`passed`件数、`failed: 0`、`skipped: 0`が必要です。古いソースや改変したreportは拒否します。
source digestはGitの未コミット・未追跡の実装ファイルも含み、vendorは固定manifestとパッチで別途検証します。

必要suiteと実API接続先、負荷条件は`config/parity/baseline.json`を参照します。
負荷の実測値には台数、同時数、実行時間、ウォームアップ、RSS増加率と、意図しない失敗・二重計上・権限漏れ・予約漏れのゼロ件数が必要です。
比較5ケースだけの成功reportを、全機能の検証済み証跡に登録してはいけません。

現在のケースと基盤試験だけを証跡に登録する場合：

```bash
python3 tools/parity/evidence.py \
  --cases-report target/parity/differential.json \
  --component-report target/parity/storage.json
python3 tools/parity/gate.py
```

両reportは同じ現ソースのdigestに一致する必要があります。登録処理は対応表の実装状態を変更しません。
ケースIDの存在・一意性・比較成功をreport本文で確認し、部分試験は`components`へ保存します。
`suites`・`live`・`load`は空のままなので、全体受け入れは引き続き失敗します。
gateはsuite名・実API接続先・負荷reportの実測値も照合し、別試験の成功reportを流用できません。
ローカル比較runnerは参照実行方式、container image ID/uv版またはPython版、Rust binaryとlockfileのSHA-256を保存します。
