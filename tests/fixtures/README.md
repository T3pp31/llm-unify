# Fixture sources

These are synthetic fixtures based on the public formats in official API documentation, rather than recordings from live services.
Verified on: 2026-10-25. Model names and response text are test data.

| File | API | Official documentation |
|---|---|---|
| openai_chat.json | Chat Completions / v1 | https://platform.openai.com/docs/api-reference/chat |
| openai_responses.json | Responses / v1 | https://platform.openai.com/docs/api-reference/responses |
| anthropic.json | Messages / 2023-06-01 | https://platform.claude.com/docs/en/api/messages |
| gemini.json | Interactions / v1beta, steps format | https://ai.google.dev/api/interactions-api |
| vertex.json | generateContent / v1 | https://cloud.google.com/vertex-ai/generative-ai/docs/model-reference/inference |

Synthetic streaming events are constructed in the contract tests.
When adding real responses, remove secrets and personal information and record the API, capture date, and source.
