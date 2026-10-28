# LLM Unifyの導入と運用

## 提供範囲

LLM UnifyはMITライセンスのゲートウェイとRust SDKです。Web画面はゲートウェイの
状態・設定モデルの閲覧、検索、モデル名とAPI Base URLのコピーを提供します。
モデル設定はYAMLで管理します。画面は上流のLiteLLM UIを使用しません。

この版は単一の管理キーを共有できる運用者向けです。利用者別の仮想キー、
テナント分離、予算による停止、利用者別の使用量集計は提供していません。
PostgreSQL/Redisの試作APIは、HTTP Proxyの課金・予算経路には接続していません。
全LiteLLM機能の互換性、管理操作、DB移行、1時間の複数台負荷試験は未完了です。
これらを必要とする環境への本番投入条件は満たしていません。

## Dockerで起動

リポジトリのルートで実行します。Docker Engine/DesktopとCompose v2が必要です。
レジストリに公開済みのイメージはまだありません。固定lockfileからローカルビルドします。

```sh
cp deploy/.env.example deploy/.env
openssl rand -hex 32
```

`deploy/.env`の次の値を設定します。ファイルをGitへ追加しないでください。

- `LLM_UNIFY_MASTER_KEY`：生成した十分にランダムな管理キー。
- `UPSTREAM_API_BASE`：接続先のAPI Base URL。通常は末尾が`/v1`のURL。
- `UPSTREAM_API_KEY`：接続先のキー。認証不要のローカル接続でも空値にはせず任意の非secret文字列を指定。
- `PROXY_BIND_ADDRESS`・`PROXY_PORT`：公開するアドレスとポート。既定はlocalhostのみ。
- その他の制限・ビルド並列数・ヘルスチェック値は同ファイルに集約。

`deploy/proxy.yaml`の`litellm_params.model`を接続先モデルへ変更します。
`model_name: main`は利用者に公開する名前で、クライアントのmodel欄に指定します。
非対応・不明なYAML設定を指定した場合、起動は失敗します。
ローカルLLMがホスト上で稼働するDocker Desktop環境では、接続URLのホストに
`host.docker.internal`を使用します。Linuxではホスト向けの接続経路を別途設定してください。

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml config --quiet
docker compose --env-file deploy/.env -f deploy/compose.yaml build
docker compose --env-file deploy/.env -f deploy/compose.yaml up -d
docker compose --env-file deploy/.env -f deploy/compose.yaml ps
```

既定ポートなら`http://127.0.0.1:4000/ui/`を開き、管理キーでログインします。
このイメージにはUIが埋め込まれ、Node.js・Pythonの本番実行環境は不要です。
非root・read-only filesystem・権限削除・PID/メモリ制限で実行します。
YAMLは読み取り専用でマウントし、再起動して反映します。
`deploy/.env`と設定ファイルは起動前に読み取り可能な権限で作成してください。

## 外部からのアクセス

TLSは信頼できるリバースプロキシまたはロードバランサーで終端します。
ブラウザが表示するoriginがAPI Base URLになります。パスを追加せず同じoriginの
`/ui/`、`/ui/api/`、`/v1/`、`/health/`を転送し、Authorizationヘッダーを保持してください。
HTTPSを通して管理キーと推論リクエストを送信します。
管理画面を運用者に限定する場合は、入口でネットワーク/認証による制限も設定します。
ゲートウェイに予算・利用者別制限が実装されていると仮定して公開しないでください。

UIのキーはメモリ内のみで使い、ログアウト・ページ離脱で破棄します。
ページを再読み込みすると再ログインが必要です。ブラウザストレージには保存しません。

## ヘルスチェックとログ

- `/health/liveliness`：プロセスのHTTP応答。
- `/health/readiness`：設定・認証初期化後、リクエストを受理可能。上流サービスの正常性を保証するものではありません。
- 上流・モデルの正常性は、その接続先が対応するAPIに実リクエストを送り検証します。

Composeはログをローテーションします。環境変数やCompose展開結果にはsecretが含まれるため、
`docker compose config`の全文や`docker inspect`の環境変数を公開ログへ貼り付けないでください。

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml logs --tail 100 proxy
```

## 更新・切り戻し

1. 現在のイメージタグまたはdigest、YAML、`deploy/.env`をアクセス制限した場所へ退避します。
2. 新しいソースを別のチェックアウトでビルドし、別ポート・別の試験専用キーでAPIとUIを確認します。
3. `LLM_UNIFY_IMAGE`を新タグへ変更し、`up -d`で切り替えます。実行中のリクエストを終了する時間は`PROXY_STOP_GRACE_PERIOD`で設定します。
4. 異常があれば以前のイメージタグと設定へ戻し、`up -d --no-build`で再作成します。

現Proxyは使用量DBを使用しません。設定・secret・イメージを運用上の復元対象としてください。
別コマンド`llm-unify-gateway`のSQLite台帳のバックアップは、このProxy構成と混同しないでください。
LiteLLM PostgreSQL/SQLiteからの移行コマンドは未提供です。

停止・再開：

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml stop
docker compose --env-file deploy/.env -f deploy/compose.yaml up -d
```

## 検証とリリース

`python3 tools/check_container.py --image llm-unify:0.1.0`で、実際のComposeを使う
隔離スモーク試験を行います。認証・秘密情報の非公開、UI、readiness、非root・read-only実行、
再起動後の回復を確認し、試験専用コンテナだけを削除します。上流への推論は実行しません。
比較・実API・移行・負荷を含む全体の受け入れは`docs/parity/README.md`を参照してください。
Dockerスモーク試験の成功だけを全機能の製品受け入れとして扱いません。

## ライセンス

[MIT License](../LICENSE)。上流Rustの出典・変更履歴・ライセンスは
[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)を参照してください。
