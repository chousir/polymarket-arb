// Token Bucket 速率限制器 + 指數退避
// POST /order: 60/s 均速
// GET /books: 5/10s（嚴格限制）
// 429 回應時指數退避：1s → 2s → 4s + jitter
