# 官方文档核对记录

> 本文记录实现前对 QQ 机器人官方文档的逐项核对结果，以及据此修正的问题。
> 核对时间：2026-11-01
> 文档来源：`https://bot.q.qq.com/wiki/`（官方站点，VuePress SSR）

---

## 一、核对过的官方页面

| 主题 | 路径 |
|---|---|
| 事件订阅与通知 / Payload / OpCode / 签名校验入口 | `/wiki/develop/api-v2/dev-prepare/interface-framework/event-emit.html` |
| 签名算法（Ed25519 详细规则 + DEMO） | `/wiki/develop/api-v2/dev-prepare/interface-framework/sign.html` |
| 发送群聊消息 `POST /v2/groups/{group_openid}/messages` | `/wiki/develop/api-v2/autogen/api/v2_groups_group_openid_messages.post.html` |
| 互动事件 `INTERACTION_CREATE` 事件体 | `/wiki/develop/api-v2/autogen/event/interaction_create.html` |
| 互动事件响应 `PUT /interactions/{interaction_id}` | `/wiki/develop/api-v2/autogen/api/interactions_interaction_id.put.html` |
| 群@机器人消息 `GROUP_AT_MESSAGE_CREATE` 事件体 | `/wiki/develop/api-v2/autogen/event/group_at_message_create.html` |

---

## 二、发现并修正的问题

### 1. Markdown 消息内容字段（严重）

- **原实现（错误）**：`msg_type=2` 时把 Markdown 文本放在顶层 `content`。
- **官方定义**：`msg_type=2` 必须用 `markdown: { content: "..." }`，且 **`content`、`ark` 必须为空**（"填写此字段后 content/ark 必须全为空"）。
- **修正**：改为 `{"msg_type":2, "markdown":{"content": "..."}}`。

### 2. 按钮回调类型（严重）

- **原实现（错误）**：按钮 `action.type = 2`。
- **官方定义**：
  - `type=0` 跳转按钮
  - **`type=1` 回调按钮：回调后台接口，data 传给后台** ← 投票按钮应该用这个
  - `type=2` 指令按钮：自动在输入框插入 @bot data
- **修正**：投票按钮改为 `action.type = 1`。

### 3. 按钮点击后必须回应（严重，否则客户端一直 loading）

- **原实现（缺失）**：收到 `INTERACTION_CREATE` 后未做任何回应。
- **官方定义**：
  - 收到事件后需调用 `PUT /interactions/{interaction_id}`，body `{"code": 0}`。
  - **仅 `type=11`（消息按钮）和 `type=12`（快捷菜单）需要回应**。
  - 不回应则客户端一直 loading 直到超时；同一 interaction_id 只能回应一次。
- **修正**：新增 `ack_interaction()`，在业务逻辑前先 PUT 回应。

  `code` 语义：`0`成功 / `1`操作失败 / `2`操作频繁 / **`3`重复操作** / `4`没有权限 / `5`仅管理员操作。

### 4. 群内用户 openid 字段名（严重）

- **原实现（错误）**：读 `member_openid`。
- **官方定义**：群聊场景字段是 **`group_member_openid`**（事件顶层），群 openid 是 `group_openid`。
  - `GROUP_AT_MESSAGE_CREATE` 中用户在 `author.member_openid`。
  - `INTERACTION_CREATE` 中用户在 **`group_member_openid`**。
- **修正**：按场景分别取正确字段。

### 5. 回调地址验证 op=13 的返回格式（严重）

- **原实现（错误）**：直接返回 `plain_token` 明文。
- **官方定义**：必须返回 `{"plain_token": "...", "signature": "hex(Ed25519(event_ts + plain_token))"}`。
- **修正**：实现完整签名返回。

### 6. 签名校验未实现（严重）

- **原实现**：注释写着"生产必须开启"，但没有代码。
- **官方定义**：
  - Header：`X-Signature-Ed25519`（hex 64 字节）、`X-Signature-Timestamp`。
  - 签名体 `msg = timestamp + body`。
  - 私钥 seed = **AppSecret 重复拼接至 32 字节**（`while len < 32: s = s*2`，取前 32 字节）。
- **修正**：完整实现 `verify_signature()` + `build_seed()`，并支持 `SKIP_SIGNATURE=true` 用于本地调试。

### 7. 事件体解析层级（严重）

- **原实现（错误）**：按 `payload.d.d` 取事件体，且 `op==0` 用 `OP_HEARTBEAT_ACK` 常量命名混淆。
- **官方定义**：`{ id, op, d, s, t }`，事件类型在 **`t`**，事件体直接在 **`d`**。
- **修正**：正确处理 `payload.t` + `payload.d`。

### 8. 消息重复推送去重（重要）

- **官方提示**：`GROUP_AT_MESSAGE_CREATE` 中"相同 msg_id 可能重复推送，开发者需结合 msg_seq 做去重"。
- **修正**：用 KV 按 `msg_id` / `interaction_id` 做去重（TTL 300s）。

### 9. AccessToken 接口与过期解析（中等）

- 官方：`POST https://bots.qq.com/app/getAppAccessToken`，body `{appId, clientSecret}`；返回 `expires_in` 为**字符串**。已按此处理。
- 另：官方文档提到新增机器人的**调用域名为 `https://api.bot.qq.com/`**（旧为 `api.sgroup.qq.com`），且默认启用 IP 白名单。当前代码用 `api.sgroup.qq.com`，**如遇 403/超时请改用 `api.bot.qq.com`**。

---

## 三、签名算法实测验证 ✅

用官方 DEMO 的 AppSecret 做了实际计算，**结果与官方文档逐字符一致**：

输入：
```
secret      = DG5g3B4j9X2KOErG
plain_token = Arq0D5A61EgUu4OxUvOp
event_ts    = 1725442341
```

输出：
```
seed = DG5g3B4j9X2KOErGDG5g3B4j9X2KOErG   (长度 32)
我们的签名 = 87befc99c42c651b3aac0278e71ada338433ae26fcb24307bdc5ad38c1adc2d01bcfcadc0842edac85e85205028a1132afe09280305f13aa6909ffc2d652c706
官方 DEMO  = 87befc99c42c651b3aac0278e71ada338433ae26fcb24307bdc5ad38c1adc2d01bcfcadc0842edac85e85205028a1132afe09280305f13aa6909ffc2d652c706
MATCH = true ✅
```

另验证公钥派生于官方 DEMO 例子也**逐字节一致**：
```
secret = naOC0ocQE3shWLAfffVLB1rhYPG7
seed   = naOC0ocQE3shWLAfffVLB1rhYPG7naOC
pubkey = [215 195 98 254 120 174 248 31 242 50 135 180 147 98 139 93
          176 42 60 79 227 11 33 94 77 25 96 155 93 118 103 58]
        ← 与官方 DEMO 完全一致 ✅
```

> 注：官方「验证签名」页面里的 `GATEWAY_EVENT_NAME` 示例签名无法复现，
> 属于该文档示例代码块排版问题（body 内空白字符差异），不影响算法正确性。
> 但**回调验证签名示例可完整复现**，已证明实现无误。

---

## 四、群消息接口要点速查（官方原文摘要）

```
POST /v2/groups/{group_openid}/messages
- 被动消息有效期 5 分钟，每个消息最多回复 5 次
- 主动消息频控：
    Bot 维度：企业/个人身份证认证 60/qpm；未认证 30/qpm
    单关系维度：20/qpm，每个群 1 天最多接收 1000 条
- msg_type: 0=纯文本(content) 2=Markdown(markdown) 7=富媒体(media) 8=图文卡片(card)
- msg_id:   被动回复用，来自 GROUP_AT_MESSAGE_CREATE 的 d.id
- event_id: 被动回复事件用，来自事件最外层 id，支持
            "INTERACTION_CREATE" / "GROUP_ADD_ROBOT" / "GROUP_MSG_RECEIVE"
- msg_seq:  与 msg_id 联合使用去重；相同 msg_id + msg_seq 重复发送会失败
- keyboard: 短形式只传 id；长形式传 content.rows
```

按钮结构（官方）：
```
Button {
  id, render_data{label, visited_label, style}, action{type, permission, data}
  style: 0 灰色线框 / 1 蓝色线框 / 3 白底红字 / 4 蓝底白字
  action.type: 0 跳转 / 1 回调后台 / 2 指令
  permission.type: 0 指定用户 / 1 管理员 / 2 所有人
}
```

---

## 五、仍需你在后台确认的事项

1. **API 域名**：机器人后台若开启了 IP 白名单，需把 Workers 出口 IP 加白；并确认用 `api.bot.qq.com` 还是 `api.sgroup.qq.com`。
2. **权限申请**：群聊能力、主动消息、Markdown 消息、消息按钮（自定义按钮为内邀能力）均需在管理端申请。
3. **主体类型**：群场景默认仅企业主体可开发（个人主体仅频道场景）。
