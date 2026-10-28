# 固定したLiteLLM参照ソース

`litellm/` は `config/parity/baseline.json` で指定したアーカイブから取り込みます。
上流コードと管理UIはMITライセンスで、著作権表示は `litellm/LICENSE` に保持します。
`enterprise/` は取り込みません。UIに同梱された依存物のライセンスも元のまま保持します。

`python3 tools/parity/source.py import` で取り込み、`verify` で全ファイルのSHA-256を検証します。
元のディレクトリ配置を保ち、Rustの相対参照と上流の比較テスト用データを維持しています。
`litellm/SOURCE.json` に取り込んだ全ファイルのハッシュを保存します。
上流への変更は `config/parity/patches.json` に理由・変更前後のSHA-256・一意の置換内容を記録し、
取り込み時に適用します。記録のない直接編集は `verify` が拒否します。
`SOURCE.json` のハッシュは元のアーカイブ内容のまま保持します。

現在のパッチはRust framerとLLMテスト依存の `aws-smithy-eventstream =0.61.4` を `^0.61.4` に緩和するものです。
既存SDKが必要とする0.61.6と解決できるようにし、Bedrock SDKの意図しない降格を防ぎます。
