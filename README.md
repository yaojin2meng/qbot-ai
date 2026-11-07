# qbot-ai

QQ 官方机器人（Cloudflare Workers + TypeScript），具备：

1. **模型 API 测速**：`测速 <Base URL> <API Key>` → 自动拉模型列表 → 用随机题测试（防空回）→ 回一份测速报告。
2. **群内投票**：`投票 <主题> <预计人数>` → 发出带「赞成/反对/弃权」按钮的消息 → 每人一票 → 达标自动结算汇总。
3. **Web 配置台**：网页内完成首次初始化、凭据自检、一键验证。

> ⚠️ 各接口的字段与签名细节均按 QQ 官方文档核对过，详见 **[docs/OFFICIAL_DOC_CHECK.md](docs/OFFICIAL_DOC_CHECK.md)**。

## 技术栈

- **TypeScript** → 由 wrangler 打包，运行在 Cloudflare Workers（**没有 Rust / wasm 构建步骤**）
- Cloudflare **D1**（SQLite）存投票、配置与幂等记录
- Cloudflare **KV** 缓存 Bot AccessToken
- Cloudflare **Queues** 承接事件，先入队再 ACK
- **WebCrypto Ed25519** 实现回调验签与 `op=13` 回调地址验证（无第三方加密库）

产物体积：**57 KiB / gzip 17 KiB**，免费版 Workers（1 MiB gzip）绰绰有余。

## 目录结构

```
wrangler.toml          Workers / D1 / KV / Queues 绑定
package.json           依赖与脚本
src/
  index.ts             入口：路由 + 队列消费者
  schema.ts            数据库结构（唯一的 DDL 来源，自动建表）
  db.ts                首次请求自动建表 / 版本标记
  qq.ts                Ed25519 验签、op13 验证、事件派发、幂等消费
  qqApi.ts             QQ OpenAPI 封装、AccessToken 缓存、凭据自检
  poll.ts              投票指令解析 / 创建 / 按钮回调 / 结算
  modelTest.ts         URL 校验、随机题测速、并发池、报告生成
  config.ts            配置读取（D1 → secret/vars）、状态自检、首次初始化
  security.ts          D1 租约锁（去重 + 限流）
  http.ts              带超时与体积上限的 JSON 请求
  env.ts               Worker 绑定类型
web/index.html         配置台（构建时内嵌进 Worker）
```

## 部署步骤

### 1. 安装依赖
```bash
npm install
```

### 2. 登录 Cloudflare
```bash
npx wrangler login
```

### 3. 创建 D1 与 KV，并填进 `wrangler.toml`
```bash
npx wrangler d1 create qbot          # 把 database_id 填进 wrangler.toml
npx wrangler kv namespace create KV  # 把 id 填进 wrangler.toml
npx wrangler queues create qqbot-events
```

### 4. 部署
```bash
npx wrangler deploy
```

**不需要手动建表。** 部署完成后第一次访问（网页、Webhook 或队列消费）时，
Worker 会自己执行 `src/schema.ts` 里的 DDL 把表建出来，并往 `settings`
写一条 `schema_version`。之后每次冷启动只做一次只读版本探测，不重复建表。

想确认建好了：访问 `https://<你的域名>/api/status`，返回里
`"schema": {"version": 1, "ready": true}` 就对了。

（旧版本靠 `wrangler d1 migrations apply`，现在 `migrations/` 目录已删除，
schema 只在 `src/schema.ts` 一处维护。已经用旧 migration 建过表的库也能直接
跑，缺的列会自动补上、脏计数会按明细表重算。）

### 5. QQ 开放平台配置
- 回调地址：`https://<你的域名>/qq/webhook`
- 订阅事件：`GROUP_AT_MESSAGE_CREATE`、`INTERACTION_CREATE`
- 需申请权限：群聊能力、主动消息、Markdown 消息、消息按钮（自定义按钮为内邀能力）

### 6. 打开配置台
访问 `https://<你的域名>/`，填 AppID / AppSecret。**只需填一次**，之后配置台变为只读。

也可以不用网页，直接下发 secret：
```bash
npx wrangler secret put QQ_APP_ID
npx wrangler secret put QQ_APP_SECRET
```
## 群内使用

**必须 @ 机器人**，否则 QQ 不会推送 `GROUP_AT_MESSAGE_CREATE` 事件，机器人收不到任何东西。

```
投票 周末去爬山吗 10
测速 https://api.deepseek.com/v1 sk-xxxx
帮助
```

测速还支持直接粘 new-api / one-api 的连接信息（带 `url` + `key` 字段的 JSON），不用加「测速」前缀，会自动识别并开始测速：

```json
{"_type":"newapi_channel_conn","key":"sk-xxxx","url":"https://your-relay.example.com"}
```


## 关键实现说明

| 项 | 正确做法 |
|---|---|
| Markdown 消息 | `msg_type=2` + `markdown:{content}`，顶层 `content` 必须为空 |
| 投票按钮 | `action.type=1`（回调后台），**不是** 2（指令按钮） |
| 按钮点击回应 | 必须 `PUT /interactions/{id}` body `{"code":0}`，否则客户端一直 loading |
| 群用户字段 | `INTERACTION_CREATE` 用 `group_member_openid`；群@消息用 `author.member_openid` |
| 测速报告 | 走**主动消息 + Markdown**（不带 `msg_id`），不受 5 分钟被动回复窗口限制；发送失败自动回退纯文本被动回复 |
| 投票计数 | 落票 + 从明细表重算 + 满员关票在同一个 D1 `batch` 事务里完成，不依赖触发器 |
| 回调地址验证 op=13 | 返回 `{plain_token, signature}`，signature=`hex(Ed25519(event_ts+plain_token))` |
| 验签 | `X-Signature-Ed25519` / `X-Signature-Timestamp`，签名体=`timestamp+body`，seed=`AppSecret` 重复至 32 字节 |
| 事件解析 | 事件类型在 `payload.t`，事件体在 `payload.d` |
| 消息去重 | 相同 msg_id 会重复推送，用 `webhook_events` + 租约锁去重 |

### 签名算法已实测验证 ✅

```
secret=DG5g3B4j9X2KOErG, plain_token=Arq0D5A61EgUu4OxUvOp, event_ts=1725442341
WebCrypto 算出的签名与官方 DEMO 逐字符一致 (MATCH=true)
```

## 注意事项

- **调试可跳过验签**：在 D1 `settings` 表里写 `SKIP_SIGNATURE`=`true`，生产务必删掉。
- **API 域名**：代码用 `api.sgroup.qq.com`；若后台开了 IP 白名单或报 403，把 `QQ_API_BASE` 改成 `https://api.bot.qq.com`。
- **主动消息配额**：Bot 维度 60/qpm（认证）或 30/qpm（未认证）；单群 20/qpm、每天最多 1000 条。
- **并发测速上限**：单次最多 30 个模型，并发 5，单个超时 15 秒。
- **主体要求**：群场景默认仅企业主体可开发，个人主体仅频道场景。
- **关于出图**：早期版本用 Rust + resvg 渲染 PNG 报告，wasm 有 4.4 MB（gzip 1.19 MiB），
  超出免费版 Workers 的 1 MiB 上限，已改为纯文本报告。若确实需要图片，可升级到付费版
  （10 MiB 上限）后引入 `workers-og` 之类方案。
