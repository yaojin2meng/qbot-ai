-- 运行时配置表。
-- 网页配置台里填的值存这里，优先级高于 wrangler secret / [vars]。
-- 清空某一项 = 回落到 wrangler secret / vars / 内置默认值。
CREATE TABLE IF NOT EXISTS settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
