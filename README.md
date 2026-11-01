# qbot-ai

QQ 官方机器人（Rust + Cloudflare Workers），具备：
1. **模型 API 测速**：填入 Base URL + Key → 自动拉取模型列表 → 用随机题测试（防空回）→ 生成测速报告图片。
2. **群内投票**：`投票 <主题> <预计人数>` → 发出带「赞成/反对/弃权」按钮的消息 → 每人一票 → 达标自动结算汇总。
3. **Web UI 管理台**：配置 API、一键测速看图、查看投票记录。

> ⚠️ 本项目已按 QQ 官方文档逐项核对并修正（含签名算法实测比对通过），详见 **[docs/OFFICIAL_DOC_CHECK.md](docs/OFFICIAL_DOC_CHECK.md)**。

## 技术栈
- Rust → WebAssembly，运行在 Cloudflare Workers
- Cloudflare D1（SQLite）存投票与配置
- Cloudflare KV 缓存 Bot AccessToken + 消息去重
- 纯 Rust `resvg` + `tiny-skia` 渲染 SVG→PNG 出图
- `ed25519-dalek` 实现回调 Ed25519 验签

## 目录结构
```
Cargo.toml                     依赖与构建配置
wrangler.toml                  Workers / D1 / KV 绑定
migrations/0001_init.sql       数据库建表
docs/OFFICIAL_DOC_CHECK.md     官方文档核对与修正记录 ⭐
src/lib.rs                     路由入口
src/qq.rs                      Ed25519 验签 + op13 验证 + 事件派发 + 去重
src/poll.rs                    投票指令解析/创建/按钮回调 ACK/去重/结算
src/model_test.rs              模型拉取 + 随机题测速 + 防空回
src/render.rs                  测速结果 SVG→PNG 出图
src/admin.rs                   配置增删查、投票记录
web/index.html                 管理台（内嵌进 Worker）
```

## 部署步骤

### 1. 准备工具
```bash
npm i -g wrangler
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup target add wasm32-unknown-unknown
cargo install worker-build
```

### 2. 登录 Cloudflare
```bash
wrangler login
```

### 3. 创建 D1 与 KV
```bash
wrangler d1 create qbot-db
# 把输出 database_id 填进 wrangler.toml

wrangler kv namespace create KV
# 把输出 id 填进 wrangler.toml
```

### 4. 初始化数据库
```bash
wrangler d1 execute qbot-db --file=./migrations/0001_init.sql --remote
```

### 5. 配置密钥
```bash
wrangler secret put QQ_APP_ID
wrangler secret put QQ_APP_SECRET      # 同时用于 AccessToken 获取和回调验签
```

### 6. 部署
```bash
wrangler deploy
```

### 7. QQ 开放平台配置
- 回调地址：`https://<你的域名>/qq/webhook`
- 订阅事件：`GROUP_AT_MESSAGE_CREATE`、`INTERACTION_CREATE`
- 需申请权限：群聊能力、主动消息、Markdown 消息、消息按钮（自定义按钮为内邀能力）

### 8. 打开管理台
访问 `https://<你的域名>/` 即可管理 API 配置与查看投票。

## 群内使用
```
投票 周末去爬山吗 10
```
机器人发出带三按钮的卡片；每人限一票；满 10 人自动结算并汇总。

## 关键实现说明（已按官方文档修正）

| 项 | 正确做法 |
|---|---|
| Markdown 消息 | `msg_type=2` + `markdown:{content}`，顶层 `content` 必须为空 |
| 投票按钮 | `action.type=1`（回调后台），**不是** 2（指令按钮） |
| 按钮点击回应 | 必须 `PUT /interactions/{id}` body `{"code":0}`，否则客户端一直 loading |
| 群用户字段 | `INTERACTION_CREATE` 用 `group_member_openid`；群@消息用 `author.member_openid` |
| 回调地址验证 op=13 | 返回 `{plain_token, signature}`，signature=`hex(Ed25519(event_ts+plain_token))` |
| 验签 | `X-Signature-Ed25519`/`X-Signature-Timestamp`，签名体=`timestamp+body`，seed=`AppSecret` 重复至 32 字节 |
| 事件解析 | 事件类型在 `payload.t`，事件体在 `payload.d` |
| 消息去重 | 相同 msg_id 会重复推送，用 KV 去重 |

### 签名算法已实测验证 ✅
```
secret=DG5g3B4j9X2KOErG, plain_token=Arq0D5A61EgUu4OxUvOp, event_ts=1725442341
生成的签名与官方 DEMO 逐字符完全一致 (MATCH=true)
```

## 注意事项
- **调试可跳过验签**：`wrangler.toml` 里 `SKIP_SIGNATURE="true"`，生产务必改回 `"false"`。
- **API 域名**：代码用 `api.sgroup.qq.com`；若后台开启了 IP 白名单或报 403，改用 `https://api.bot.qq.com`。
- **Worker 出图字体**：Workers 无系统字体，中文可能缺字；渲染失败时自动降级为前端表格。要稳定出图需内嵌字体。
- **主动消息配额**：Bot 维度 60/qpm（认证）或 30/qpm（未认证）；单群 20/qpm、每天最多 1000 条。
- **并发测速上限**：单次最多 30 个模型（`model_test.rs` 的 `.take(30)`）。
- **主体要求**：群场景默认仅企业主体可开发，个人主体仅频道场景。