# Fixture の出典

これらは実サービスから取得した記録ではなく、公式 API の公開形式に基づく合成 Fixture です。
確認日: 2026-10-25。モデル名と本文はテスト用です。

| ファイル | API | 公式資料 |
|---|---|---|
| openai_chat.json | Chat Completions / v1 | https://platform.openai.com/docs/api-reference/chat |
| openai_responses.json | Responses / v1 | https://platform.openai.com/docs/api-reference/responses |
| anthropic.json | Messages / 2023-06-01 | https://platform.claude.com/docs/en/api/messages |
| gemini.json | Interactions / v1beta、steps 形式 | https://ai.google.dev/api/interactions-api |
| vertex.json | generateContent / v1 | https://cloud.google.com/vertex-ai/generative-ai/docs/model-reference/inference |

Streaming の合成イベントは契約テスト内で組み立てています。
本物の応答を追加するときは、秘密情報と個人情報を除去し、API・取得日・出典を記録してください。
