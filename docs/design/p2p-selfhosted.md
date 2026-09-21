# 自建 P2P 网络设计（Self-hosted Network）

| 状态 | 最后更新 |
| --- | --- |
| 提案（RFC，未定案） | 2026-09-21 |

> **本文的定位**：本文是 `p2p-technical.md`（下称"既有设计"）的一次**定位级改版提案**，不是补丁。既有设计隐含"由单一运营方提供一张全局 P2P 网络"的假设——Tracker 是信任根、状态存在 Tracker 自己手里、运营方承担带宽与中继成本。本文把假设换成：**任何人都可以创建一个 P2P 网络，网络的权威状态存放在创建者自己的对象存储（桶）里**。
>
> 数据面（ICE / DTLS / WebRTC DataChannel、block / piece / chunk、Merkle 证明、调度）几乎不变，本文只做摘要并指向既有设计；**变化集中在控制面与信任模型**。第 10 章给出 v2 → v3 的完整差异清单，第 11 章列出尚未定案的问题。

## 1. 引言

### 1.1 动机

既有设计解决了"两个 Peer 如何安全地交换数据"，但它的部署模型是单租户的：一个 Tracker 集群、一份身份注册表、一张资源索引、一个运营方。这带来三个绕不开的问题：

1. **运营方承担全部基础设施成本**——状态存储、Tracker 计算、TURN 中继带宽都由运营方付钱，而带宽成本正是产品想摊给用户的东西（见 `p2p-product.md` §9）。
2. **计量缺乏可信基准**——SDK 嵌在客户 App 内、客户是计费对象，`min(上报)` 这类交叉校验防不住两端合谋少报（`p2p-product.md` §8.3 的问题，已由 #9 记录）。
3. **元数据集中在运营方**——谁在分享什么、谁在线，全部由运营方持有。这既是合规负担，也让"自建社区"这类场景无法成立。

把网络的所有权交还给创建者，三者同时得到解决：**状态与带宽落在创建者自己的桶上**，而桶的出向流量由云厂商计量——一个外部、不可篡改、per-network 的计费基准。

### 1.2 与既有设计的关系

| 维度 | 既有设计（v2） | 本文（v3） |
| --- | --- | --- |
| 网络规模假设 | 单张全局网络 | 任意多张网络，各自独立 |
| 权威状态存放 | Tracker 的共享存储 | 创建者的对象存储桶 |
| 信任根 | Tracker Server | 网络创建者（桶），Tracker 是被授权签发者 |
| Tracker 状态 | 有持久状态，多实例需共享存储 | 无持久业务状态，多实例无需共享存储 |
| 吊销范围 | 全局 | 网络级 |
| 计量基准 | 遥测交叉校验 / 第三方 CDN 账单降幅 | 桶出向流量（云厂商账单）+ A/B 对照 |
| Punch 提供者 | 运营方 | 运营方公共池，或创建者自建 |
| 数据面 | Merkle + chunk + 双 DataChannel | **不变** |

### 1.3 规范性语言

本文按 RFC 2119 / RFC 8174 的语义使用 **必须**（MUST）、**不得**（MUST NOT）、**应当**（SHOULD）、**可以**（MAY）。

### 1.4 角色与术语（新增与变更）

沿用既有设计的 `peer_id`、block / piece / chunk、Binding 等术语，新增：

- **网络创建者（Network Creator）**：创建一个网络的人。他提供桶、决定授权哪些 Tracker 与 Punch、对**自己网络内**的元数据拥有完全控制权。他不是全局管理员——他的权力只覆盖自己的网络。
- **网络（Network）**：由 `network_id` 标识的一个命名空间。同一个 `peer_id` 可以加入任意多个网络，彼此独立。
- **桶（Bucket）**：网络的权威存储，S3 兼容对象存储（AWS S3、Cloudflare R2、MinIO、腾讯云 COS 等均可）。既存资源字节（CDN 源），也存网络元数据。
- **网络描述符（Descriptor）**：桶里的一份文档，描述网络的名字、授权的 Tracker 公钥集合、Punch 白名单等。

角色关系：

```mermaid
graph LR
    C[网络创建者] -->|拥有并可直接修改| B[(桶 = 网络权威)]
    C -->|授权| T[Tracker]
    C -->|白名单| P[Punch Server]
    T -->|读写元数据| B
    P -->|只读 吊销列表/清单| B
    Peer -->|register / login / query| T
    Peer -->|文件字节 / leaf_hashes| B
    Peer <-->|DTLS 直连| Peer
```

## 2. 网络模型

### 2.1 网络的定义

一个网络由它的桶唯一确定。**`network_id` 就是规范化后的桶 URI**：

```
network_id = "s3://my-bucket/net/demo"
```

这样定义的好处是它自解释、无需额外哈希、客户端拿到就能定位权威状态；代价是**换桶等于换网络**（需要重新分发种子），这是可接受的——换存储位置本来就是重建网络级别的操作。

桶里必须存在一份网络描述符 `net/descriptor.json`：

```text
{
  v: 1
  network_id: "s3://my-bucket/net/demo"
  name: "...", description: "..."        展示用
  created_at: <unix seconds>
  creator_public_key: <32B>              创建者公钥，用于验签本文件与 trackers.json
  signature: <64B>                       sign(creator_key, canonical(本文件除 signature 外))
}
```

描述符是**网络的根信任锚**：客户端首次加入一个网络时下载它、校验创建者签名，之后把 `creator_public_key` 作为该网络一切元数据变更的验证依据。

### 2.2 信任根的迁移

这是本文最重要的一处语义变化。既有设计 §8 写"Tracker 是控制面的信任根"；本文改为：

> **网络创建者是信任根。Tracker 只是被创建者授权的签发者与门房。**

具体含义：

| 能力 | 归属 |
| --- | --- |
| 决定谁能当 Tracker | 创建者（`net/trackers.json`） |
| 决定谁能当 Punch | 创建者（`net/punches.json`） |
| 吊销某个 peer | 创建者（直接改桶）或 Tracker（代写） |
| 签发票据 | Tracker（用 `net/trackers.json` 里的密钥） |
| 撤销某个 Tracker | 创建者（从 `net/trackers.json` 移除 + 撤销其桶授权） |

后果是**Tracker 作恶的破坏半径变小了**：它不能篡改创建者未授权给它写的前缀（见 3.5 节的最小权限），也不能阻止创建者把它踢掉。这是 self-hosted 模型相对单租户模型最实质的安全收益。

### 2.3 客户端如何加入一个网络

种子文件与磁力链不变（既有设计 §5），只是 `tracker_list` 与 `cdn_list` 的语义具体化：

```text
种子文件 = bencode({
  tracker_list: ["https://t1.example.com", "https://t2.example.com"]   该网络的 Tracker，可多个
  cdn_list:     ["https://my-bucket.s3.amazonaws.com/net/demo/files/x.iso"]
  proof_list:   ["https://my-bucket.s3.amazonaws.com/net/demo/files/x.iso.hashes"]
  leaf_hashes:  <bytes>                                    可选，内联叶子哈希列表
  info: { v: 2, name, length, block_size, piece_size, mime, root }
})
```

`cdn_list` 与 `proof_list` 默认指向桶——这是本模型与既有设计的自然契合点：创建者把文件放进桶，桶同时就是冷启动的 CDN 源与证明源，全网零 seeder 也能工作（既有设计 §5.1 的 `proof_list` 硬约束在此变成默认形态）。

一个 `info_hash` 在多个网络中是相同的（它只是内容的哈希），因此**同一份内容可以在不同网络间被发现**——跨网络互通是未来可做的能力，本文不定义。

### 2.4 网络生命周期

| 阶段 | 动作 |
| --- | --- |
| 创建 | 创建者建桶、写 `net/descriptor.json`、放文件与叶子哈希列表、向 Tracker 与 Punch 授权 |
| 运营 | 创建者增删 Tracker / Punch、查看统计、封禁 peer（写吊销分片） |
| 迁移 | 换桶 = 新 `network_id`，需重新分发种子；旧网络可保留只读一段时间 |
| 消亡 | 创建者删除桶即可；客户端表现为"网络不可用"，**应当**给出明确错误而非静默重试 |

## 3. 桶：布局与一致性

### 3.1 对象布局

```text
net/descriptor.json                          网络描述符（创建者签名，根信任锚）
net/trackers.json                            授权 Tracker 公钥集合（JWKS，创建者签名）
net/punches.json                             Punch 公钥白名单（创建者签名）
net/punches/state/<punch_id>.json            Punch 运行时注册（Tracker 写，含地址/容量/load）
net/revocations/manifest.json                吊销分片清单 + 各分片 ETag
net/revocations/<shard>.json                 吊销记录（按 SHA-256(public_key) 首字节分片）
net/peers/<peer_id>.json                     身份注册（含 public_key、expires_at）
net/resources/<info_hash>/<peer_id>.json     资源声明（complete、expires_at）
files/<hh>/<info_hash>                       文件字节（CDN 源）
files/<hh>/<info_hash>.hashes                叶子哈希列表（proof_list 目标）
```

`<hh>` 为 `info_hash` 前两个十六进制字符，用于打散前缀、避免单前缀热点。

### 3.2 读写频率分层

桶适合"低频、大、权威、可缓存"的数据。**决定成败的设计约束是：不要把高频写放进桶。**

| 数据 | 频率 | 放哪 | 理由 |
| --- | --- | --- | --- |
| 文件字节、`*.hashes` | 写一次，读很多 | 桶 | 天然 CDN 缓存 |
| `descriptor` / `trackers` / `punches` | 极低频写，高频读 | 桶 + CDN | 创建者控制面 |
| 吊销记录 | 极低频写，高频读 | 桶 + CDN | Punch 拉快照 |
| 身份注册 | 每 peer 每 90 天写一次，login 时读 | 桶 | 低频 |
| 资源声明 | 每 peer 每 TTL 写一次 | 桶 | **TTL 必须足够长**，见 3.4 |
| Punch 运行时状态 | 每 Punch 每 30–60 s 写一次 | 桶（数量极少，可控） | Punch 数量是十到百量级 |
| Binding / 在线状态 | 每 peer 每 10–30 min 刷新 | **Punch 内存** | 绝不能进桶 |
| `jti` 一次性消费 | 每次连接 | **Punch 内存** | `aud` 已绑定单实例 |
| login nonce | 每次 login | **无状态**（见 4.4） | 不做存储 |
| 限流计数 | 每请求 | **Tracker 内存**（可丢失） | 见 7.3 |

### 3.3 一致性、并发与原子性

现代对象存储对 PUT / GET / LIST 都是写后读一致（S3 自 2020 年起，R2、MinIO 等同样），因此"Tracker 写完 Punch 立刻能读到"成立。仍需注意：

1. **没有事务**。跨对象的原子性不存在，只能靠对象拆分规避。
2. **覆盖是 last-writer-wins**。因此**资源索引必须拆成"每 peer 一对象"**——若做成单对象并发追加，两个 peer 同时 announce 会互相覆盖。
3. **条件写可用**：`If-None-Match: *` 可实现"不存在才写"（原子创建），`If-Match: <ETag>` 可实现乐观锁。这两者足以实现吊销记录的幂等写入。
4. **LIST 分页**：`net/resources/<info_hash>/` 下可能有数万个对象，query **必须**分页（每次 1000 key）并**应当**在 Tracker 侧做短 TTL 缓存。
5. **无监听机制**（对象存储没有 watch），Punch 侧一律轮询。

### 3.4 成本与容量约束

对象存储按请求计费（GET 约 $0.4/百万，PUT/LIST 约 $5/百万），量级比数据库贵、比带宽便宜。控制成本的三条硬约束：

1. **资源声明的 TTL 不得过短**。既有设计的 `ttl: 1800`（30 分钟）在桶模型下会产生显著写放大（1 万 peer 即 33 次写/秒，约 $430/月 的 PUT 费用）。本文建议 TTL **不低于 3 小时**，因为"是否在线"本就由 Punch 的 Binding 决定——**资源索引只表达"声明持有"，不表达"在线"**，失效的候选在 `signal` 阶段会被自然过滤掉。
2. **Tracker 侧缓存索引 LIST 结果**（30–60 s），把 LIST 放大压到常数级。
3. **吊销列表与描述符挂 CDN**，读流量不计入桶请求。

若网络规模大到这些约束不够用，应当引入一层缓存服务（Redis / 内存索引），但那属于优化；本文要求的是**默认配置下不触发**。

### 3.5 桶凭据与最小权限

凭据只发给服务端组件，**客户端永不得持有桶写凭据**。

| 主体 | 权限 | 范围 |
| --- | --- | --- |
| 网络创建者 | 读写全部 | 整个前缀 |
| Tracker | **读写** | `net/peers/*`、`net/resources/*`、`net/revocations/*`、`net/punches/state/*` |
| Tracker | **不得** | `net/descriptor.json`、`net/trackers.json`、`net/punches.json` |
| Punch | **只读** | `net/revocations/*`、`net/punches.json`、`net/punches/state/*` |
| 客户端 | 只读（公开桶或预签名 URL） | `files/*` |

Tracker 不能改写"谁有权签发"这件事，是 2.2 节"Tracker 作恶破坏半径变小"的技术保证。创建者通过修改桶策略即可随时撤销某个 Tracker——这使多 Tracker 与联邦得以成立（第 7 章）。

## 4. 身份与密钥

### 4.1 `peer_id`

沿用既有设计 §1.3：`peer_id = base32(SHA-256(public_key)) + bech32 校验位`，58 字符，Ed25519。**`peer_id` 是跨网络稳定的**——同一个密钥对在所有网络中是同一个身份。

### 4.2 网络内身份与吊销范围

一个 peer 要在某个网络活动，需在该网络的桶里有注册记录（`net/peers/<peer_id>.json`）。因此：

- **加入新网络 = 在该网络 register 一次**（密钥对不变，`peer_id` 不变）。
- **吊销是网络级的**：`net/revocations/*` 属于某个网络，A 网络吊销一个 peer 不影响他在 B 网络。这是 self-hosted 模型的自然结果——创建者只能管自己的网络。
- 全局吊销（"这把密钥在所有网络都失效"）本文**不定义**；跨网络的信誉与封禁属于上层能力，可用共享黑名单服务实现，但不进协议。

### 4.3 签发密钥与 `kid`

`net/trackers.json` 是该网络的 JWKS，由创建者签名：

```text
{
  v: 1
  network_id: "s3://my-bucket/net/demo"
  keys: [ { kid: "t1-2026-09", alg: "EdDSA", public_key: <32B>, created_at, status: "active" },
          { kid: "t1-2026-06", alg: "EdDSA", public_key: <32B>, status: "retired" } ]
  signature: <64B>    sign(creator_public_key, canonical(本文件除 signature 外))
}
```

每个被授权的 Tracker（可以是不同运营方）在集合里占一条 `kid`。验签方（Punch、对端 Peer）按 `kid` 取公钥。密钥轮换即追加新 `kid` 并把旧的标为 `retired`；撤销一个 Tracker 即从集合移除并标 `revoked`。

### 4.4 无状态 login nonce

既有设计要求 nonce 存于**共享存储**（多实例下存内存会失败）。在本文模型下把 nonce 存进桶既不经济也不必要，改为**自包含挑战**：

```text
GET /sessions/challenge → { challenge: <JWT> }
challenge = JWT { iss: network_id, kid, sub: peer_id,
                  scope: "login-challenge", jti: <random>,
                  exp: now + 60s }
```

客户端在 `POST /sessions` 时原样回传该 challenge，并用私钥对 `SHA-256(challenge || peer_id)` 签名。Tracker 只需**验签自己签发的 challenge**（不查任何存储）+ 验客户端签名，即可完成持钥证明与新鲜性校验。

一次性语义由挑战的短 `exp`（60 s）+ Tracker 本地极短窗口的 `jti` 去重提供——此处**允许**只存内存：即便跨实例不去重，一个 60 秒内有效的 challenge 被重放也只能让同一 peer 多拿一张等价的 session JWT，不构成权限提升。这与既有设计把 connect JWT 无状态化（#8）是同一招。

### 4.5 票据总表（相对既有设计 §6 的变化）

| JWT 用途 | `aud` | `scope` | 新增 / 变化的 claims | 有效期 |
| --- | --- | --- | --- | --- |
| Tracker 会话 | `tracker` | `query`、`announce` | `iss` = `network_id` | 分钟–小时级 |
| Punch Binding | `punch_id` | `punch` | `iss` = `network_id`；一次性 `jti` | 10–30 分钟 |
| 连接目标 Peer | `punch_b_id` | `connect` | `iss` = `network_id`；`connection_id`（= `jti`）、`grace_until` | `CONNECT_JWT_EXP`（60 s） |
| TURN 中继 | `turn_region_id` | `relay` | `iss` = `network_id`；配额 | 分钟级 |

`iss` 从"Tracker 身份"改为 **`network_id`**，签发者由 `kid` 区分。这样票据自解释"属于哪个网络、由哪把被授权的钥匙签发"，验签方从对应网络的桶取 JWKS 即可——**这正是多个独立 Tracker 能为同一网络服务的前提**。

其余校验规则沿用既有设计 §6（含所有票据**必须**校验 `iat > revoked_at`）。

## 5. 控制面

### 5.1 Tracker 的无状态化改造清单

既有设计 §7.1 说"Tracker 多实例**必须**共享状态存储"。在本文模型下这条约束删除，因为：

| 原持久状态 | 新归属 |
| --- | --- |
| 身份注册表 | 桶 `net/peers/*` |
| `revoked_keys` | 桶 `net/revocations/*` |
| 资源索引 | 桶 `net/resources/*` |
| connection 记录 | 已无状态化（既有 #8） |
| login nonce | 无状态化（4.4 节） |
| `jti` 一次性消费 | Punch 内存（`aud` 已绑定单实例） |
| Punch 清单 | 桶 `net/punches.json` + `net/punches/state/*` |

Tracker 剩下三样东西：**签名密钥**（KMS）、**限流计数**（本地、可丢失）、**索引缓存**（本地、可重建）。前两项不是"持久业务状态"，第三项丢了能重建，因此 Tracker 可以做纯粹的弹性计算。

### 5.2 Tracker REST API

路径沿用既有设计，新增网络语义与 Punch 注册端点。

| 方法 | 路径 | 作用 | 认证 |
| --- | --- | --- | --- |
| `POST` | `/peers` | register：在该网络注册 `peer_id` + 公钥 + 自签名 | 自签 |
| `DELETE` | `/peers/{peer_id}` | revoke：提交 revocation token，**在该网络内**永久吊销 | `revocation_token` |
| `GET` | `/sessions/challenge` | 取无状态挑战（4.4 节） | 无 |
| `POST` | `/sessions` | login：回传 challenge + 私钥签名 | challenge |
| `PUT` | `/peers/{peer_id}/resources` | announce：`add` / `del` 批量登记资源 | session JWT (`announce`) |
| `GET` | `/resources/{info_hash}/peers` | query：返回候选并逐个签发 connect JWT | session JWT (`query`) |
| `POST` | `/connections/relay` | relay_credentials：ICE 失败后换 TURN 凭据 | session JWT + connect JWT 本体（验 `grace_until`） |
| `PUT` | `/punch/{punch_id}` | **Punch 注册 / 续期 / 容量上报**（5.3 节） | Punch 私钥签名 + 白名单 |
| `DELETE` | `/punch/{punch_id}` | Punch 注销 | 同上 |

所有票据的 `iss` 为 `network_id`。Tracker **必须**拒绝为不属于本网络（`iss` 不匹配）的票据提供服务。

`POST /connections/relay` 与既有设计的无状态授权一致：Tracker 不保存 connection 记录，直接验 connect JWT 本体（签名 + `sub`/`target` + `info_hash` + `now <= grace_until`，**不验** `exp`）。常量关系沿用 `ICE_TIMEOUT(20s) < CONNECT_JWT_EXP(60s) < RELAY_GRACE(900s)`。

### 5.3 Punch 注册：`PUT /punch/{punch_id}`

创建者可以自己提供 Punch Server，因此这是**公开 API 而非内部运维接口**（不在路径里加 `/internal/`）。

```text
PUT /punch/{punch_id}
{
  public_key: <32B>            Punch 的长期身份公钥
  addr: "punch.example.com:3478"
  region: "ap-shanghai"
  capacity: { max_bindings: 50000, max_pending: 5000 }
  load:     { bindings: 12345, pending: 210 }     可选，供 Tracker 分配时参考
  transports: ["udp+dtls", "tcp+tls", "quic"]
  signature: <64B>             sign(punch_private_key, canonical(本文件除 signature 外))
}
```

**授权与限制**：

1. Tracker **必须**校验 `punch_id` 对应的 `public_key` 出现在桶的 `net/punches.json` 白名单里；不在白名单**不得**接受。白名单由创建者维护并签名。
2. 用该 `public_key` 验签 `signature`（持钥证明）——无需任何预共享密钥，与 `register` / `revoke` 的模式一致。
3. 注册记录带 TTL（**应当**为 120 s），重复 `PUT` 即续期；TTL 过期后 Tracker 不再把该 Punch 分配给新的 login，但已建立的 Binding 不受影响（Binding 由 Punch 自己维护）。
4. Tracker 把状态写入 `net/punches/state/<punch_id>.json`，供其他 Tracker 实例与 Punch 自身读取。
5. `DELETE` 立即删除状态记录，Tracker **应当**停止分配该 Punch；已存在的 Binding 在 TTL 内仍有效（ graceful 下线）。

**自建 Punch 的前提**：公网可达的 UDP/TCP 端口、稳定的 `punch_id`、自有 Ed25519 密钥对。文档**应当**给出容量规划口径（沿用既有设计 §6.4 的两类连接估算）。

### 5.4 Punch 信令

沿用既有设计 §3.5 / §7.2.2：`punch.join` / `punch.heartbeat` / `punch.exit` / `punch.signal`，语义、信道模型（`binding_key`、`signal_key`、常驻 / 瞬时信道的区分）全部不变。唯一变化是**吊销状态的来源**——见 5.5 节。

### 5.5 吊销分发：桶对象 + CDN 拉取

既有设计中 Punch 向 Tracker 查吊销状态，且要求 fail-closed（缓存未命中且 Tracker 不可达时拒绝）。这条依赖把 Tracker 的可用性放到了信令面的关键路径上。本文改为：

```mermaid
sequenceDiagram
    participant T as Tracker
    participant B as 桶 + CDN
    participant P as Punch Server

    Note over T: revoke：写 net/revocations/<shard>.json<br/>（条件写，幂等）
    T->>B: PUT net/revocations/manifest.json（含各分片 ETag）
    loop 每个 Punch 每 30–60 s
        P->>B: GET net/revocations/manifest.json（走 CDN）
        P->>B: GET 变更的分片
        P->>P: 更新本地 revoked 快照，记录 snapshot_at
    end
    Note over P: join / signal(offer) 命中快照即拒绝<br/>Tracker 不可达不影响判定
```

- Punch 本地维护一份 revoked 集合（按分片），`join` 与 `signal(offer)` 直接查内存。
- **陈旧度约束**：`now - snapshot_at > REVOCATION_MAX_STALE`（建议 3600 s）时，Punch **必须** fail-closed（拒绝 `join` 与 `offer`）。由于数据源是对象存储 + CDN，其可用性远高于 Tracker 服务本身，这条路径在实践中几乎不会触发——这是相对既有设计的实质改进。
- 既有设计中"revoke 时通知 Punch 清除 Binding"那条 MAY **删除**：有了轮询，它既无必要也无明确收益。
- 规模：单个网络吊销记录通常极少（密钥泄露是罕见事件），全量对象通常只有几 KB；分片方案（`revocations/<shard>.json` + manifest）只在超大规模网络才需要启用。

### 5.6 残留窗口

revoke 之后，被吊销者的残留能力（沿用既有 #19 的分析，并补充网络范围）：

1. 无法 `login`（注册记录已删）⇒ 拿不到 punch JWT、session JWT、connect JWT；
2. 已存在的 Binding 在 TTL（10–30 分钟）内仍可被找到，但**无法接受新连接**——`signal(offer)` 校验 `iat > revoked_at`，而他拿不到新的 connect JWT；
3. 已建立的 DataChannel 继续传输，直到任一端断开；
4. 残留窗口**仅限该网络**，其他网络不受影响。

## 6. 数据面

**完全沿用既有设计**：ICE + DTLS 身份锚定、block(256 KiB–4 MiB，默认 256 KiB) / piece(16 KiB) / chunk(1024 B) 三层粒度、RFC 6962 形态的 Merkle 树与 `(level, index, hash)` 证明编码、批量 pruned subtree、`request_proof` / `proof` 走可靠控制通道且先于数据、载荷含叶子哈希（对齐 BEP 52 的 `base layer = 0`）、`pause` / `resume` 取代逐条 reject、双 DataChannel 共享 SCTP 拥塞窗口的约束。

本文带来的两点自然变化：

1. **`proof_list` 的默认形态就是桶里的 `files/<hh>/<info_hash>.hashes`**——冷启动所需的"数据 + 证明"成对可得，在创建者把文件放进桶时就自动满足了。
2. **`cdn_list` 默认指向桶**。创建者**可以**在桶前挂 CDN 做加速，但那会引入一个"桶出向流量"之外的计量口径，计费上**应当**以桶账单为准（见 8.1 节）。

客户端仍按既有规则校验：`leaf_hashes` 长度必须为 `n × 32`、本地重建 root 必须等于 `info.root`，不符则拒绝；`proof_list` 缺失或重建不符时，CDN（桶）数据**不得**进入 verified 状态。

## 7. 部署与扩展

### 7.1 L1：同运营方多实例负载均衡

状态搬到桶之后，多个 Tracker 实例之间**唯一还需要共享的是签名密钥**（KMS 或安全分发）。其他：

| 项 | 多实例下的行为 |
| --- | --- |
| 身份 / 吊销 / 索引 | 桶，强一致，无同步问题 |
| nonce | 无状态（4.4 节） |
| `jti` | Punch 内存，本就无需共享 |
| 索引缓存 | 各实例本地短 TTL，陈旧可接受 |
| Punch 清单 | 桶，各实例读到同一份 |

客户端发现沿用 `tracker_list` 多地址或 DNS / anycast，无需新机制。**这是既有设计做不到、本文自然获得的能力。**

### 7.2 L2：多运营方联邦

因为授权公钥集合在桶里（`net/trackers.json`），**不同运营方可以为同一个网络签发票据**：创建者把他们的公钥加进集合即可。客户端与 Punch 按 `kid` 验签，不需要任何中心化协调。

这带来了健康的竞争与容灾：一个 Tracker 服务商下线，创建者把它从集合移除，网络照常运转。

### 7.3 唯一在多实例下变弱的环节：限流

限流计数是本地可丢失状态，跨实例不共享 ⇒ 攻击者轮流打每个实例就能绕过配额。本文**接受**这一点，并要求文档写明"限流是 per-instance 的"；若某个网络需要全局配额，需引入集中计数器（这会重新引入共享状态，属可选增强）。

### 7.4 TURN 的归属

TURN 是唯一不落在创建者桶上的基础设施成本，需要单独决策：

| 方案 | 说明 |
| --- | --- |
| 平台公共池（默认建议） | 运营方提供，成本计入订阅费，创建者无需运维 |
| 创建者自带 | 写进 `net/descriptor.json` 的 `turn_servers`，成本自担 |
| 混合 | 默认公共池，允许覆盖 |

既有 #11 记录的两个问题（coturn 原生不支持 per-connection 对端约束、relay 占比影响成本）在此不变，仍待实测。

## 8. 计量与计费

### 8.1 基准：桶出向流量

本文解决既有 #9（计量信任锚落在付钱方）的方式是把基准换成**外部数据**：桶的出向流量由云厂商计量，客户与运营方都无法单方篡改，且天然 per-network。

需要明确的是攻击方向：客户希望**少报** P2P 用量以少付费；而桶流量是云厂商出的账单，客户改不了。

### 8.2 方法一：历史基线法

```
节省量 = 基线出向流量 − 当月实际出向流量
费用   = 节省量 × 单价
```

基线取接入 P2P 之前的历史桶出向（按播放量归一化），在签约时由双方共同确认——历史账单已经产生，客户改不了。缺点：流量季节性波动会让基线失真。

### 8.3 方法二：A/B 对照法（推荐）

SDK 侧按比例分流：**对照组不启用 P2P，实验组启用**。两组的"单位播放量对应的桶出向流量"之差，就是 P2P 的真实节省率。

- 分流由**运营方**控制 ⇒ 客户无法篡改；
- 用量纲归一化（每 GB 播放的出向字节）⇒ 不受流量涨跌影响；
- 对照组比例可以很小（1–5%），成本可忽略。

这是目前能想到的最干净的计量方案：**唯一的输入是云厂商账单 + 运营方自己的分流日志**，客户端遥测不参与定金额。

### 8.4 遥测降级为归因与异常检测

既有设计 §8.2 的签名遥测保留，但用途改为：把节省量拆分到 `info_hash` / 时段 / 地域（归因），以及与桶账单长期偏离时触发人工对账（异常检测）。遥测即便被篡改，也只能影响归因精度，**不能影响金额**。

遥测字段补充 `connection_id` + `direction`（既有 #9 已要求，因 connection 记录已无状态化，配对必须由遥测自带）。

### 8.5 与既有口径的关系

既有设计（#9 修复后）定的基准是"第三方 CDN 账单降幅"。本文的"桶出向流量降幅"是同一思路的**具体化与自动化**：CDN 账单需要人工对账，桶账单可以机器读取，且 A/B 对照只能在自建桶模型下做（因为分流与数据源都在运营方可控范围内）。

## 9. 安全性考量

| 威胁 | 影响 | 缓解 |
| --- | --- | --- |
| 创建者作恶 | 审查、驱逐、窥视**本网络**元数据 | 这是他的网络，属于设计内权力；跨网络身份不受影响，用户可迁移到别的网络 |
| 桶被攻破 / 误删 | 该网络元数据不可用 | 版本控制 + 对象锁；客户端需要明确的"网络不可用"降级 |
| Tracker 被攻破 | 可签发任意票据、可写授权给它的前缀 | 最小权限（3.5 节）：改不了 `trackers.json` / `punches.json`；创建者可随时撤销其桶授权 |
| Punch 被攻破 | 信令 DoS、 Binding 伪造（无法窃听数据） | 只持只读凭据；`aud` 绑定使票据不可跨 Punch 使用 |
| 恶意 Peer 投递损坏数据 | 浪费带宽 | Merkle 校验 + 失败计数（沿用既有设计） |
| 伪造 `leaf_hashes` / `proof_list` | 试图污染数据 | 本地重建 root 与 `info.root` 比对，不符则拒绝 |
| 批量注册撑爆注册表 | 存储膨胀 | 沿用既有 #16 的建议：PoW / 邀请码 / 限流 |
| 桶凭据泄露 | 元数据被改写 | 凭据只发给服务端组件，按前缀最小授权，定期轮换 |

数据面的完整性仍然**不依赖任何服务端**：端到端 DTLS + Merkle 校验，锚点是 `info_hash` 自校验的 `info.root`。

## 10. v2 → v3 差异清单

| # | 变化 | 性质 |
| --- | --- | --- |
| 1 | 权威状态从 Tracker 存储迁到创建者的桶 | 架构 |
| 2 | 信任根从 Tracker 改为网络创建者 | 语义 |
| 3 | 引入 `network_id`，JWT 的 `iss` 改为 `network_id` | 协议 |
| 4 | 引入 `net/trackers.json`（JWKS），`kid` 承载签发者身份 | 协议 |
| 5 | 吊销范围从全局改为网络级 | 语义 |
| 6 | login nonce 改为自包含挑战，Tracker 零持久状态 | 协议 |
| 7 | Punch 吊销状态来源从 Tracker API 改为桶对象 + CDN 拉取 | 协议 |
| 8 | 删除"revoke 通知 Punch 清 Binding"这条 MAY | 简化 |
| 9 | 新增 `PUT /punch/{punch_id}` / `DELETE /punch/{punch_id}`（公开 API） | 新增 |
| 10 | Punch 清单权威从 Tracker 迁到桶 `net/punches.json` | 架构 |
| 11 | announce TTL 建议从 30 分钟放宽到 ≥ 3 小时（在线性由 Punch 承担） | 参数 |
| 12 | 计量基准改为桶出向流量 + A/B 对照，遥测降级为归因 | 产品 |
| 13 | 数据面（Merkle / chunk / 调度 / TURN 回退） | **不变** |

## 11. 未决问题

1. **`network_id` 是否直接用桶 URI**？优点是自解释、无需额外哈希；缺点是换桶等于换网络。替代方案是引入一个与位置解耦的标识 + 一层解析服务（那会引入新的中心化组件，我倾向不做）。
2. **Tracker 由平台托管多租户，还是创建者自运维**？"每个人都能创建一个网络"要求零运维，倾向平台托管 + 创建者授予桶权限；但这接受"平台被攻破影响所有网络"。
3. **公开桶还是预签名**？公开资源可以 public-read + CDN 直出（最简单，符合 P2P 分享定位）；私有网络需要 Tracker 签发预签名 URL，多一项职责与一个限流面。
4. **TURN 由谁提供**（7.4 节三选一）。
5. **跨网络发现**：相同 `info_hash` 使跨网络互通成为可能，是否要定义"联邦查询"？本文不定义，但它可能对冷启动有巨大价值（一个网络可以借用另一个网络的 seeder）。
6. **限流在 L1 多实例下的退化是否可接受**（7.3 节）。
7. **吊销列表的分片阈值**：建议先做全量对象，超过多少条才启用分片需实测。

## 12. 参考资料

- `p2p-technical.md`：既有设计（v2），本文的控制面替代方案、数据面的完整定义。
- `p2p-product.md`：产品规划（按单租户模型撰写，本文若定案需同步重写）。
- BEP 52（BitTorrent v2）：Merkle 与 `hash request` / `hashes` 消息形态的参照。
- RFC 6962（Certificate Transparency）：Merkle 树形态。
- RFC 2119 / RFC 8174：规范性语言。
