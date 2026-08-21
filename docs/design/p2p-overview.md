# P2P 网络设计

| 状态 | 最后更新 |
| --- | --- |
| 设计稿（Experimental） | 2026-08-21 |

## 1. 引言

### 1.1 本文结构

本文第 2 章给出系统总览：角色定义、控制面与数据面的分工、端到端连接建立流程。第 3 章与第 4 章分别定义控制面协议（客户端与 Tracker / Punch / STUN / TURN 的交互）与数据面协议（客户端之间的交互），两章相互独立、可按需阅读。第 5 章定义种子文件与磁力链格式，第 6 章定义统一的 JWT 授权模型，第 7 章给出接口契约与运行边界，第 8 章讨论安全性考量。初次阅读建议对照第 2.2 节的流程图；实现者优先阅读第 7 章及其下的三张客户端函数面表格。

### 1.2 规范性语言

本文按 RFC 2119 / RFC 8174 的语义使用以下关键词：**必须**（MUST）表示绝对要求，**不得**（MUST NOT）表示绝对禁止，**应当**（SHOULD）表示在特定情形下存在正当理由时可以偏离的要求，**可以**（MAY）表示真正可选的行为。

### 1.3 角色与术语

本文统一使用以下名称：

- **Tracker Server**：身份认证、资源索引、Peer 发现与 JWT 签发。
- **Punch Server**：维护 Peer 的在线 Binding，转发 WebRTC 信令（SDP 与 ICE candidate）。
- **STUN Server**：供 ICE 收集公网映射候选地址。
- **TURN / Relay Server**：无法直连时经 ICE relay candidate 转发数据流。

**`peer_id`**：由客户端长期保存的 Ed25519 公钥派生的可读字符串，格式为 `peer:` + base32(SHA-256(public_key)) + bech32 校验位，约 58 字符，例如 `peer:ABCDEF234567...`。客户端本地生成密钥对、不由 Tracker 分配——密钥即身份，Tracker 只是发现与授权的控制面（密钥轮换即更换身份，见第 3.1 节）。base32 字母表为 RFC 4648（不含 `0/O/1/I` 以避免视觉混淆），bech32 校验位（BCH 码）可检测 4 位以内的转录错误。`peer_id` 同时用于协议消息、UI 显示、数据库主键——不再区分"二进制 ID"与"可读编码"。

**Ed25519**：本设计全程使用 Ed25519 作为签名算法。选择理由：公钥仅 32 字节、签名 64 字节、验签速度比 RSA-2048 快 10–50 倍（对每次 `join` 都要签名的移动端场景关键）、抗侧信道攻击。本设计**不**使用 RSA、ECDSA 或 GPG/OpenPGP 密钥格式——客户端自己生成并保存 Ed25519 密钥对，不依赖外部密钥管理工具。

## 2. 系统总览

### 2.1 控制面与数据面

整个系统分为两条面：

| | 控制面 | 数据面 |
| --- | --- | --- |
| 参与方 | 客户端 ↔ Tracker / Punch / STUN / TURN | 客户端 ↔ 客户端（直连） |
| 职责 | 身份与授权（JWT）、在线 Binding、资源索引（announce / query）、连接信令（SDP 与 ICE candidate 中继）、TURN 授权 | 连接建立与身份锚定（ICE / DTLS）、数据组织（block / piece）、块可见性（bitfield / have）、传输（request / reject / piece / cancel）、取块与上传调度 |
| 消息形态 | 请求-响应对（Punch 侧另有常驻 / 瞬时信道） | 对称单向消息 |
| 信任基础 | 短命 JWT、持钥证明与 MAC | 端到端 DTLS 与 block 哈希校验 |

分界一句话：控制面回答"你是谁、有什么、可以连谁"，数据面回答"怎么连上、怎么把数据传对"。服务端不参与文件数据：Tracker 只维护 peer_id → info_hash 映射与 `complete` 标志，块级信息只在直连双方之间交换。数据面统一构建在 WebRTC DataChannel 之上：ICE 负责打洞与连通性，DTLS 负责加密与身份，SCTP 负责消息传输；浏览器与 App 共用同一栈。

### 2.2 连接建立全流程

下图展示一次完整下载从上线到数据传输的全过程，横跨控制面与数据面。

```mermaid
sequenceDiagram
    autonumber
    participant A as Client A（下载方）
    participant STUN as STUN Server
    participant T as Tracker Server（JWT 签发方）
    participant PA as Punch A
    participant PB as Punch B
    participant B as Client B（提供方）
    participant TURN as TURN / Relay Server

    par A 上线
        A->>T: register(peer_id, public_key, signature)  [首次]
        T-->>A: 注册确认
        A->>T: login(peer_id, nonce_signature)
        T-->>A: session JWT；punch JWT（aud=Punch A，scope=punch）；STUN/TURN 配置
        A->>PA: join(punch JWT, proof_of_possession)
        PA-->>A: binding_id、binding_key、expires_at
        A->>STUN: Binding Request
        STUN-->>A: A candidates（host / srflx）
    and B 上线并登记资源
        B->>T: register(peer_id, public_key, signature)  [首次]
        T-->>B: 注册确认
        B->>T: login(peer_id, nonce_signature)
        T-->>B: session JWT；punch JWT（aud=Punch B，scope=punch）；STUN/TURN 配置
        B->>PB: join(punch JWT, proof_of_possession)
        PB-->>B: binding_id、binding_key、expires_at
        B->>STUN: Binding Request
        STUN-->>B: B candidates（host / srflx）
        B->>T: announce(info_hash, complete, TTL)
    end

    Note over A,PB: A / B 定期调用 heartbeat(binding_id, seq, MAC)，刷新 Binding 与 NAT 映射

    A->>T: query(info_hash)
    T-->>A: B peer_id、公钥、Punch B 地址、connect JWT（target_peer_id=B、connection_id）
    Note over T,A: 不返回 B 的公网地址或候选地址；connection_id 由 Tracker 生成并签入 JWT，签发时记录 connection_id → (A, B, info_hash)

    A->>PB: signal(type=offer, connection_id, SDP offer + A candidates, connect JWT)
    PB->>PB: 原子登记 connection_id（一次性）；验签与 claims 校验
    PB-->>A: 首个 signal 的响应（含 signal_key）
    PB->>B: signal(type=offer, connection_id, info_hash, A peer_id, connect JWT, SDP offer + A candidates)
    B->>PB: signal(type=answer, connection_id, SDP answer + B candidates)
    PB->>PB: 检查 B 仍处于有效 Binding
    PB-->>A: signal(type=answer, connection_id, B peer_id, SDP answer + B candidates)

    par ICE 连通性检查
        A->>B: ICE checks + DTLS 握手
    and
        B->>A: ICE checks + DTLS 握手
    end
    Note over A,B: 身份锚定：对端 DTLS 证书指纹与信令中 SDP 一致；connect JWT 的 sub 与 sub_pub 派生关系一致

    alt 直连成功
        A<<->>B: DataChannel 建立（控制 reliable+ordered / 数据 unreliable+unordered）
        opt magnet 模式（缺 info 的一端）
            A->>B: request_info
            B-->>A: info（校验 SHA-256 == info_hash）
        end
        A->>B: bitfield（verified block 位图，为空可跳过）
        B->>A: bitfield
        loop 下载循环（顺序窗口 / rarest first 选 block，pipelining 维持在途请求）
            A->>B: request(block_index, piece_offset, length)
            B-->>A: piece（走数据通道）或 reject（退避 / 转投）
        end
        Note over A,B: block 集齐 → SHA-256 校验 → verified → 广播 have；哈希不匹配 → 回 partial 重下并计失败
    else 直连失败
        Note over T: 依据签发 connect JWT 时的 connection 记录校验请求方身份
        A->>T: relay_credentials(connection_id)
        T-->>A: A 的 TURN REST 凭据
        B->>T: relay_credentials(connection_id)
        T-->>B: B 的 TURN REST 凭据
        A<<->>TURN: 加密数据流
        TURN<<->>B: 加密数据流
    end
```

## 3. 控制面协议

本章描述客户端与 Tracker / Punch / STUN / TURN Server 之间的交互：身份建立、在线状态、资源发现、信令交换与中继授权。

### 3.1 登录与身份建立

#### 3.1.1 密钥生成与注册

客户端首次启动时本地生成 Ed25519 密钥对（私钥永不外传，**应当**以操作系统提供的密钥存储机制保管，如 macOS Keychain / Android Keystore / iOS Secure Enclave）。`peer_id` 由公钥派生：`peer_id = SHA-256(public_key)`（32 字节）。

客户端向 Tracker 发起一次性的 `register` 请求，建立 `peer_id → public_key` 的全网唯一映射：

```
register(peer_id, public_key, signature, timestamp)
  where signature = sign(private_key, "register" || peer_id || public_key || timestamp)
```

Tracker 验证三项后**必须**入库：

1. `peer_id == SHA-256(public_key)` —— 防止伪造 `peer_id` 指向他人的公钥。
2. `verify(public_key, signature)` —— 确认请求方确实持有对应私钥。
3. `peer_id` 未被注册，或原注册记录已过期（见下文 TTL）。

注册记录带 TTL（**应当**为 90 天，可配置），到期后 `peer_id` 可被重新注册。peer **必须**在 TTL 内 `login` 续期；续期时 Tracker **应当**刷新 TTL，连续活跃的 peer 无需重复 `register`。

#### 3.1.2 登录

`login` 是会话级认证，建立 Tracker 与 peer 之间的短期会话 JWT。客户端携带公钥及对挑战的签名登录：

```
login(peer_id, nonce_signature)
  where nonce_signature = sign(private_key, "login" || peer_id || tracker_nonce || timestamp)
```

`tracker_nonce` 是 Tracker 先前下发的一次性随机数（登录页面或上一次 session 末尾下发），**必须**一次性消费。Tracker 查注册表取出 `public_key` 验签，验证通过后返回本次会话可使用的 JWT、分配的 Punch Server 地址，以及 STUN/TURN 配置。

`login` 同时承担注册续期：成功 `login` 等同于刷新 `register` 的 TTL。

#### 3.1.3 设计意图

这样设计的目的，是让 `peer_id` 保持长期稳定，同时让短期访问权可过期、可轮换。Tracker 不是文件数据的中转站，而是身份、发现与授权控制面。

`peer_id` 派生自公钥意味着密钥轮换即更换身份：客户端生成新密钥对、以新 `peer_id` 重新 `register` + `login` 并 `announce`，新旧身份短暂并存完成迁移。本设计不做签名链——各层票据的短有效期（connect 数十秒、Binding 10–30 分钟、session 小时级）已把单把密钥泄露的损失框在有限时间内，复杂度不匹配收益。

#### 3.1.4 密钥吊销（Revocation）

私钥泄露时，短 `exp` 只能把损失框在有限窗口内，无法立即止血。本设计引入 revocation token 机制：

客户端在 `register` 时**必须**同时生成 revocation token，并离线保存：

```
revocation_token = sign(private_key, "revoke" || peer_id || timestamp)
```

客户端**应当**将此 token 导出为本地文件或托管到可信第三方。当私钥泄露或疑似泄露时，任何持有 token 的人可向 Tracker 提交：

```
revoke(peer_id, revocation_token)
```

Tracker 验签后，将 `peer_id` 加入 `revoked` 列表（带吊销时间戳）。此后任何针对该 `peer_id` 的 `login` / `join` 请求**必须**拒绝；Punch Server 在 `join` 时**应当**通过 Tracker 的公开 API 校验 `peer_id` 是否被吊销（可短时间缓存以降低延迟），或依赖 session JWT 上的吊销标记。

`revoked` 列表带 TTL（**应当**为 7 天，与 register TTL 解耦）。TTL 到期后 `peer_id` 可被重新注册——这平衡了"吊销有效性"与"peer_id 永久占用"。Token 丢失时只能等 register TTL 过期后重新注册（期间身份仍可被冒充，因此客户端**应当**妥善备份 token）。

#### 3.1.5 客户端 UI 中的 peer_id

客户端 UI **应当**在"我的身份"页面显示：

- `peer_id`（完整字符串）：用于复制、搜索、添加好友、扫码
- 二维码：编码 `peer_id`，方便线下扫码添加

`peer_id` 本身就是可读字符串，无需额外编码或截断。用户复制粘贴、二维码扫描、搜索框输入都用同一个值。

### 3.2 Binding：`join`、`heartbeat` 与 `exit`

客户端使用仅面向自身 Punch Server 的 `punch JWT` 建立 Binding，并使用 `peer_id` 对应私钥对本次请求的 nonce、`punch_id`、时间戳签名，证明令牌持有者确实控制该身份。单独的 bearer JWT **不得**作为 join 的唯一凭据；nonce **必须**一次性消费（Punch Server 维护覆盖时间窗口两倍时长的去重记录），时间戳超出短窗口的请求**必须**拒绝，否则被截获的 join 请求在 JWT 有效期内可被重放。Punch Server 验签并验证该证明后创建 `binding_id`，并通过该加密信道的已认证响应返回随机生成的会话级 `binding_key`。此 Binding 代表"该 peer 当前可通过这个 Punch Server 被联系到"。

之后的保活使用 `heartbeat(binding_id, seq, MAC)`，而不是每次发送完整 JWT。MAC 固定为 `HMAC-SHA-256(binding_key, binding_id || seq || timestamp || transport_generation)`；服务端仅接受递增序列号（允许一个有限乱序窗口）且 timestamp 在短窗口内的请求。Binding 到期、客户端重新 join、或网络切换时，旧 `binding_key` 和旧序列号空间立即失效。这样既刷新 NAT 映射和在线 TTL，也避免频繁验签与较大的 UDP 包。

主动下线使用 `exit(binding_id, seq, MAC)`，复用 heartbeat 的 MAC 与序列号机制，无需新凭据；成功后 Binding 立即删除、`binding_key` 作废。客户端下线时**应当**先通过 `announce` 的 `del` 撤下自己的全部资源。Tracker 不提供 session 吊销：JWT 无状态，短 `exp` 已把泄露损失框在有限时间内，"下线"的语义由 `exit` 与资源撤下共同承担。

### 3.3 ICE 候选收集

候选地址由 ICE agent 收集：host candidate（局域网地址，**应当**以 mDNS 名称暴露）与 srflx candidate（经 STUN 映射的公网地址）。连通性检查、角色仲裁（controlling / controlled）、candidate 配对与 consent 保活均由 ICE 完成，客户端无需自行实现同时探测；网络切换通过 ICE restart 触发新一轮收集与检查。

Tracker 不保存或下发这些地址：候选地址会随网络切换而变化，且提前暴露会扩大隐私泄露与扫描风险。候选地址只在获得连接授权后的 Punch 信令中交换（trickle ICE）。candidate 使用标准 ICE candidate 编码，携带优先级与 `transport_generation`（ICE restart 时递增）；generation 变化后旧 candidate **不得**使用。默认不对非同一私网的 Peer 发送 host candidate，以避免泄露内网拓扑；允许发送时**应当**使用 mDNS 名称或由用户显式选择。

### 3.4 资源宣告与查询

B 使用 `announce` 登记自己可提供的资源：可一次 `add` 一组 `info_hash`（各带 `complete` 标志、TTL），或一次 `del` 一组 `info_hash` 主动撤下索引（不必等 TTL 过期）；记录**必须**带 TTL（服务端设置最大值），离线后自动失效。A 使用 `query(info_hash)` 查询少量候选 Peer。`info_hash` 固定为 info 的 SHA-256 摘要，info 的编码与字段见第 5 节。`complete` 标记是否持有整个文件，供 query 做粗粒度候选筛选（避开与自己没有交集的 Peer，降低 connect JWT 的无效消耗）；块级信息不上报 Tracker——Tracker 只维护 peer_id → info_hash 映射与 `complete` 标志，不保存全局块位图，细粒度的块可见性与稀缺度属于数据面，由直连双方交换维护。**不得**使用调用方自定义的文件名或非规范哈希作为资源标识。

Tracker 的查询结果仅包含 B 的 `peer_id`、B 的身份公钥、B 所在 Punch Server 地址和一次连接所需的 `connect` JWT；**不包含** B 的公网地址或候选地址。`connect` JWT 以自身的一次性 `jti` 作为 `connection_id`（由 Tracker 生成、不可预测）；Tracker 在签发时记录 `connection_id → (A, B, info_hash, 创建时间)`，TTL 为连接建立超时，作为后续 `relay_credentials` 的授权依据。**应当**限制候选数量、对 query/announce 按身份和资源施加速率限制，并限制单个资源的返回分页，以避免热门资源的索引放大和完整 Peer 列表泄露。

### 3.5 Punch 信令交换

A 使用 `connect` JWT 中的 `connection_id`（即该 JWT 的一次性 `jti`，由 Tracker 生成、不可预测），并通过 `punch.signal` 发送 `type=offer`，其中包含自己的 SDP offer（含 DTLS 证书指纹与 ICE 用户名/口令）、候选地址与 `connect` JWT。Punch B 验证 `aud`、`scope`、`sub`、`sub_pub`、`target_peer_id`、`info_hash`、`connection_id`、`exp` 后，原子登记该 `connection_id`（即消费掉这个一次性 `jti`），并在该请求的响应中下发 `signal_key`（见下文），再向 B 转发该 offer（含 `info_hash`、`connection_id` 与 connect JWT 本体），使 B 能在应答前基于文件决定接受或拒绝，并能独立验签 JWT、校验 `target_peer_id` 与 JWT 中 `sub_pub`（A 公钥）的派生关系。B 再通过同一个 `punch.signal` 接口发送 `type=answer`，携带 SDP answer（含 B 的 DTLS 证书指纹）、最新候选与 `connection_id`；Punch B 仅在该记录仍有效且 B 处于有效 Binding 时将其转发给 A。

同一 `connection_id` 的连接记录有效期内，相同 offer 的重传**必须**幂等，并返回已有处理结果；不同内容复用同一 `connection_id` **必须**拒绝。若在连接建立超时前未收到 answer，A **应当**重新 `query` 获取新的 `connect` JWT 与新的 `connection_id` 发起新连接。`cancel` 会使 Punch B 删除该连接记录、停止转发，并向对端转发一条 `type=cancel`，对端收到后立即清理本地状态；未收到通知的一端仍**应当**在固定连接超时后清理记录。离线、令牌过期、目标拒绝、限流**应当**返回可区分的错误码，但对未授权请求不泄露目标是否在线。

`offer`、`answer`、`candidate` 和 `cancel` 都是 `punch.signal` 的消息类型，而不是独立接口。`offer` / `answer` 承载 SDP，Punch Server 将其视为不透明数据中继，**不得**解析；`candidate` 承载标准 ICE candidate 字符串（trickle ICE），均使用同一个 `connection_id`。SDP 中的 DTLS 证书指纹经 Punch 认证信道送达，构成端到端身份校验的锚点（见第 4.1 节）。

因此，A 直接请求 **Punch B**，而不是先经由 Punch A 再转发。Punch A 的职责仅是维护 A 自己的在线 Binding；只有当其他 Peer 主动连接 A 时，Punch A 才进入信令路径——本流程中 A 是发起方，Punch A 不出现。Punch B 则因为持有 B 的有效 Binding，能够把信令送达 B。

A 一侧的信令走**连接范围的瞬时加密信道**：随 A 的首个 `signal`（offer）建立，生命周期与 Punch B 上的连接记录一致（连接建立成功或超时即关闭），answer、trickle candidate、cancel 与错误都经它双向传递。首个 `signal` 以 connect JWT 认证；此后同一信道上的消息不再重复验证 JWT——信道即凭据，relay 回退阶段的 candidate 交换因此不受 JWT `exp` 限制。Punch B 在首个 `signal` 的响应中同时下发随机生成的 `signal_key`（对称密钥，只在加密信道内出现，有效期与连接记录一致）；这与 Binding 的模式同构：punch JWT 之于 `binding_key`，正如 connect JWT 之于 `signal_key`。若瞬时信道意外断开，A 重连 Punch B 并出示 `connection_id` 与 `signal_key` 即可恢复该连接的信令；Punch B 在连接记录中保留已收到但未送达 A 的信令（通常是 answer 与少量 candidate），重连后按序补发，送达或超时后丢弃。

B 一侧的信令送达依赖 B 与 Punch B 之间在 join 时建立的常驻加密信道：heartbeat 同时为该信道保活；信道断开即视为 Binding 失效，Punch Server 对后续连接请求返回可区分的"目标不可达"错误。两类信道的具体传输均不作限定，TCP+TLS、UDP+DTLS 或 QUIC 均可，硬性要求相同——机密性与完整性（`binding_key`、`signal_key` 与 connect JWT 均只经此类信道传输）、服务器认证（防止假 Punch Server 接管信令面）、以及网络切换后的可恢复性（常驻信道凭重新 join 与 `transport_generation` 语义恢复，瞬时信道凭 `signal_key` 重连）。

这也是部署前提：Punch Server 的容量需按两类连接估算——面向 B 的常驻 Binding 数（小时级），以及面向 A 的挂起瞬时信道数（数十秒级，受 connect JWT 有效期与连接超时共同框定），并对单个 peer/IP 的并发挂起信道数设上限，防止批量挂连接耗尽资源。

## 4. 数据面协议

本章描述连接两端 Peer 之间的交互：ICE 连通性与端到端认证、数据组织（block 与 piece）、info 拉取（magnet 模式）、块可见性交换（bitfield 与 have）、piece 级请求与传输、block 选择与上传调度，以及直连失败时的 TURN 回退。

### 4.1 ICE 连通性与端到端认证

拿到彼此 SDP 与候选地址后，ICE agent 自动完成连通性检查（同时向对方候选发包建立 NAT 映射、角色仲裁、提名），DTLS 在选中的路径上完成加密握手并建立 SCTP 关联。打洞与握手本身无需自行设计，需要设计的是**身份锚定**：

- offer / answer 中的 DTLS 证书指纹经 Punch 认证信道送达（A 侧凭 connect JWT 建立的瞬时信道，B 侧凭 Binding 常驻信道）；连接建立时双方**必须**校验对端实际 DTLS 证书与信令中指纹一致，防止信令之后的路径替换。
- B 侧**必须**校验 Punch 转发的 offer 所附 connect JWT（Tracker 验签、`target_peer_id` 为自己、`info_hash` 一致），并确认 JWT 中 `sub_pub`（A 公钥）能派生出 JWT 的 `sub`；A 侧确认 answer 经 B 的有效 Binding 送达，且 Tracker 在 query 结果中返回的 B 公钥能派生出目标 `peer_id`。

连通性检查失败或超时按第 3.5 节的重试规则处理；candidate 失效或网络切换通过 ICE restart 恢复（对应 `transport_generation` 递增）。直连失败时进入第 4.7 节的 TURN 回退。

连接建立后，文件数据走 DataChannel，Punch Server 不参与文件传输。数据的组织、校验、可见性与传输见后续各节。

### 4.2 数据组织：block 与 piece

数据组织分为两个粒度，分别服务校验成本与传输流水线：

- **block：校验单位。** info 内嵌每个 block 的 SHA-256（格式见第 5 节），逐 block 校验即查表。BitTorrent v1 的 pieces 与 v2 的 piece layers 同为全量哈希列表；注意命名相反——BT 称该哈希单位为 piece，其 wire 消息 `piece` 装载的却是本设计的 piece，本设计的命名与 BT wire 层一致。block 大小**必须**是 piece 大小的整数倍，典型 256 KiB–4 MiB，最后一个 block 可以短于标准大小。客户端只对集齐全部 piece 的 block 计算哈希；校验通过才进入 verified 状态，也只有 verified 的 block 可以响应他人的请求。单个 piece 损坏最多作废一个 block，端到端校验使恶意 Peer 无法污染数据。
- **piece：传输单位。** 固定 16 KiB（2^14 字节），文件末尾的 piece 可以短。请求、重传与取消都以 piece 为粒度；不对单个 piece 做哈希校验。

block 的生命周期为 missing → partial → downloaded → verified 四态：partial 状态下为该 block 维护 piece 级位图；集齐全部 piece 进入 downloaded，哈希匹配进入 verified，不匹配则回到 partial 重新请求，并对提供错误数据的 Peer 记一次失败（失败计数达到阈值后断开并在一段时间内拒绝重连）。持久化采用 in-place 写入：piece 直接写最终 offset，不做"验证后再落盘"的二次拷贝；重启后对未 verified 的 block 重新校验，能通过哈希的保留 partial 进度，其余丢弃。info 面向单文件：一个 `info_hash` 对应一个文件、一张位图；多文件分发由多个 `info_hash` 并存表达（各自独立切分与校验），不引入跨文件的 block 组织。

### 4.3 info 拉取（magnet 模式）

分享可以只传一个 `info_hash`：Tracker 地址既可来自磁力链的 `tr`、完整种子文件的 `tracker_list`，也可来自客户端默认配置，三个来源合并去重、依次尝试（见第 5 节）。`query(info_hash)` 无需 info 即可找到 Peer（Tracker 只索引 `info_hash`，不需要其内容）。磁力链的格式与字段语义见第 5.3 节。

连接建立后，缺 info 的一端在交换 bitfield 之前发送 `request_info`，对端回复 `info`（info 字节串，**必须**遵守实现配置的大小上限）；收到方**必须**校验 `SHA-256(info) == info_hash` 后才能使用，因此提供伪造 info 的 Peer 只能浪费对方一次往返，不构成信任问题。两端都已持有 info 时跳过。`request_info` / `info` 走控制通道。

### 4.4 块可见性：bitfield 与 have

块级可见性只在直连双方之间维护，Tracker 只保存 `complete` 粗粒度标志（见第 3.4 节）。ICE/DTLS 完成、DataChannel 建立、双方 info 就绪（magnet 模式下先完成拉取，见第 4.3 节）后、发送任何数据请求前，双方交换 bitfield：对每个 `info_hash` 一张 block 级位图，长度为该 info 的 block 数（按 bit 向上取整，高位补零）。bitfield **只能**包含 verified 的 block；谎报无法获利——对端请求时要么交不出数据计入失败，要么伪造数据过不了 block 哈希。

此后每验证一个 block，向当前保持连接的所有 Peer 广播一条 `have(block_index)`。可见性信息不持久化，重连后重新交换 bitfield。位图体积为 block 数 / 8 字节（1 GiB 文件、256 KiB block 即 512 字节），随握手一次性传输；超大文件的分段位图属后续优化，本设计不做。

### 4.5 请求与传输

每个连接开两条 DataChannel：

- **控制通道**：reliable + ordered，承载 `bitfield`、`have`、`request`、`reject`、`cancel`、`request_info`、`info`；
- **数据通道**：unreliable + unordered（`maxRetransmits = 0`），只承载 `piece`。丢块不做传输层重传，由应用层的在途超时重新 `request` 或转投其他 Peer 兜底——慢 Peer 的丢包不产生任何队头阻塞。

DataChannel 是消息语义（SCTP message），每条协议消息即一个消息，无需自定义长度前缀帧；16 KiB piece 远低于消息大小上限，无需分片。请求与响应均为 piece 粒度：

- `request(block_index, piece_offset, length)`：`length` ≤ 16 KiB。同一连接维持固定数量的在途请求（pipelining，典型 5–16 个），使吞吐不受 RTT 限制；在途请求由发送方跟踪，超时未响应即重新 `request` 或转向其他 Peer。
- `reject`：`request` 的两个响应分支之一，上传方即时、按单个请求粒度拒绝（带宽不足、调度优先级低、请求不合法）；下载方收到后退避重试或转投其他 Peer。
- `cancel(block_index, piece_offset, length)`：撤销尚在途的请求。
- `piece`：`request` 的数据响应分支，携带 `block_index`、`piece_offset` 与数据，接收方写入后更新对应 block 的 piece 位图。

上传方对 pending request 按本地调度策略排序服务；请求方在途请求数超出协商窗口视为协议违规，**可以**断开连接。下载进入尾声（剩余 block 的全部 piece 均已在途）时进入 endgame 模式：向所有持有者重复请求同一 piece，先到先用，其余以 `cancel` 撤销，避免个别慢 Peer 拖住整体完成时间。

### 4.6 block 选择与上传调度

下载侧按以下优先级选择 block，信息来源为已交换的 bitfield，属局部视野（Tracker 不提供全局稀缺度）：

1. **顺序窗口（sequential window）**：流式播放时，以播放头为起点的有限窗口（按播放时长配置，典型 30–60 秒对应的 block 数）内的 block 严格按序、最高优先级，保证边下边播；
2. **strict block**：已开始的 block 优先下完，控制 partial block 的内存占用；
3. **rarest first**：未开始的 block 中优先选择已见持有者最少的，维持冷门 block 的副本数；
4. **random first**：刚加入时前几个 block 随机选择，尽快产生第一个 verified block，从纯下载方转为可上传方。

非流式下载等价于顺序窗口为空——同一套机制，不引入模式开关。流式场景的观看完成率低，长尾 block 更容易随 Peer 离场而稀缺，窗口外的 rarest first 因此更重要而非更不重要。窗口大小是唯一权衡：过小则网络抖动直接转化为卡顿，过大则趋近纯顺序下载、削弱 swarm 健康度。

上传侧不设显式的 choke / interested 状态机：`request` 本身即兴趣声明，`reject` 即反馈，互惠调度退化为纯本地策略——pending request 优先服务近期向自己上传过数据的 Peer，并保留轮换探索名额（等价 BitTorrent 的乐观 unchoke：被拒的下载方退避后偶尔重试新 Peer）。调度策略实现可替换（例如换成中心化调度），不影响协议互操作。

### 4.7 TURN 回退

对称 NAT、企业防火墙或 CGNAT 等环境可能导致直连失败。ICE 在连通性检查未通过时自动引入 relay candidate：双方各自调用 `relay_credentials(connection_id)`，Tracker 依据签发 connect JWT 时记录的 `connection_id → (A, B, info_hash)` 校验请求方是该连接的一端、记录仍在 TTL 内（记录 TTL 为连接建立超时，独立于 connect JWT 的 `exp`，因为走到 relay 回退时 JWT 通常已过期），随后换发短期 TURN REST 凭据（coturn 的 static-auth-secret HMAC 方案，username 编码过期时间与配额）；若 TURN 实现支持 JWT 鉴权，也可以直接签发 `relay` JWT。relay 候选地址经 `punch.signal` 的 `type=candidate` trickle 交换，连接记录在超时前**不得**清理。

TURN 只转发密文流量（DTLS），不能成为内容可信边界。凭据只发给请求方自身；TURN 或其前置授权网关**必须**只允许该连接的配对端点，若所选标准 TURN 实现无法约束 peer 地址，则**必须**由前置网关实施该策略。

## 5. 种子文件与磁力链

### 5.1 种子文件结构

种子文件采用 envelope 结构：`tracker_list` 携带 Tracker 地址并位于 `info` 之外——Tracker 是基础设施部署细节，**不得**参与内容身份，换 Tracker 或增设备用地址不改变 `info_hash`（BT v1 将 announce 放在 info dict 之外正是为此；BT 的 announce / announce-list 双字段源于历史兼容，本设计不采用，统一为单一扁平列表）。`info` 与 `info_hash` 采用 bencode 编码：字典 key 强制排序、整数十进制、字符串带长度前缀，同一逻辑内容在任何实现下编码出的字节串唯一，`info_hash = SHA-256(canonical info 字节串)` 因此稳定。达到同等性质需要 RFC 8785 级别的 JSON 规范化，本设计不采用。结构如下：

```text
种子文件 = bencode({
  tracker_list: ["https://tracker1.example.com",
                 "https://tracker2.example.com"]   Tracker 地址列表，不参与哈希；可空（依赖客户端配置）
  cdn_list: ["https://cdn.example.com/files/x.iso",
             "https://cdn2.example.com/x.iso"]   可选，CDN 兜底源，不参与哈希；可空
  info: {                                        内容身份，被哈希
    v: 1                                         info 格式版本，当前必须为 1
    name: "..."                                  可选，建议文件名，仅展示用，不参与身份
    length: <u64>                                文件字节数
    block_size: <u32>                            校验块大小，2 的幂，256 KiB–4 MiB
    piece_size: <u32>                            可选，传输单位，默认 16384；block_size 必须是其整数倍
    mime: "..."                                  可选，流式场景的内容类型
    hashes: [<32B> × ceil(length / block_size)]  每 block 的 SHA-256
  }
})
info_hash = SHA-256(canonical info 字节串)
```

`cdn_list` 指向承载同一字节内容的 HTTP(S) 地址，客户端将其当作**拥有全部 block 的虚拟 Peer**：block 请求映射为 HTTP Range 请求，响应数据照常经 info 哈希校验，因此无需信任 CDN 本身（对应 BT 的 web seeding，BEP 19）。CDN 的用量与优先级——P2P 优先、CDN 兜底；流式场景顺序窗口可 CDN 优先保播放体验——是纯本地策略，不占数据面消息集。CDN 同时回答了冷启动：全新文件的第一个完整副本由发布者置于 CDN 即可，无需任何初始 seeder；swarm 衰竭（Peer 走光、TTL 过期）时 CDN 同样兜底。

Tracker 不存储种子文件与 info——与 BT 的 tracker 不存 .torrent 一致。分享有两种形态：完整种子文件自包含、双击即用（`tracker_list` 即 Tracker）；只分享 `info_hash`（magnet 模式）时，Tracker 来自客户端默认配置。两个来源合并去重、依次尝试（列表顺序即偏好），天然 failover；`tracker_list` 长度上限由实现配置。

### 5.2 info 校验与解析规则

- `v` 位于 info 内部：版本决定哈希覆盖字段的解释方式，**必须**参与哈希——否则旧客户端可能在新格式上通过哈希校验并静默误读（BT v1 的 info dict 无版本字段，v1/v2 割裂即其后果）。envelope 层不设版本号，其字段误读是良性的（连错 Tracker → query 失败 → failover），未知字段忽略已覆盖演进。
- `hashes` 长度**必须**等于 `ceil(length / block_size)`；最后一个 block 可短于 `block_size`，其哈希按实际长度计算。
- `block_size` **必须**为 2 的幂且在 256 KiB–4 MiB 内；`piece_size` **必须**整除 `block_size`。
- 解析前**必须**检查种子文件总长上限（实现可配置），防御恶意巨型文件的 DoS。
- 未知字段忽略以保持向前兼容，但 `info_hash` 始终是对 canonical info 字节串的 SHA-256，生成方只**应当**写入规范定义的字段。

哈希以完整列表内嵌（1 GiB 文件、256 KiB block 约 128 KiB），逐 block 校验为 O(1) 查表；更紧凑的 Merkle 树证明形态留作后续优化，当前不引入。

### 5.3 磁力链格式

```text
magnet:?xt=urn:sha256:<info_hash 的 64 位小写 hex>&dn=<显示名>&tr=<Tracker 地址>
```

scheme 复用通用的 `magnet:`（BT 未发明专属 scheme，复用的正是 magnet 生态的注册处理器；身份区分由 `xt` 的 namespace 承担）。namespace 即哈希算法：值 = 该算法对 canonical info 字节串的摘要，当前为 `sha256`——算法演进时直接换 namespace（如 `urn:sha512:`），不为新算法发明新命名空间（`btih` → `btmh` 的迁移成本不复现）。注意：magnet 生态中 `urn:sha1` 等 namespace 在 Gnutella 时代指文件内容本身的哈希，本设计的哈希对象是 info 字节串，语义以本规范为准；BT 客户端忽略非 `btih` 的 `xt`，不产生互通歧义。

`xt` 是唯一规范字段——URN 而非 URL：命名内容而非位置（P2P 的前提是同一内容多源，身份必须与位置解耦）。`dn`、`tr` 为可选提示：`dn` 的权威来源是 `info.name`（链接可伪造，info 自校验），`tr` 缺省时使用客户端默认配置。可选字段错误或缺失不影响正确性。

## 6. 授权模型：JWT 的统一模型

所有授权票据可使用同一种 JWT 格式与同一套 Tracker 签名密钥体系，但**必须**签发为不同、最小权限的 JWT 实例。

| JWT 用途 | `aud` | `scope` | 关键附加 claims | 典型有效期 |
| --- | --- | --- | --- | --- |
| Tracker API 会话 | `tracker` | `query`、`announce` | `sub` | 分钟级至小时级 |
| 建立/刷新 Punch Binding | 指定 `punch_id` | `punch` | `sub` | 10–30 分钟 |
| 请求连接目标 Peer | 指定 `punch_b_id` | `connect` | `sub`、`sub_pub`（发起方公钥）、`target_peer_id`、`info_hash`、`connection_id`（即一次性 `jti`） | 数十秒 |
| TURN 中继授权 | 指定 `turn_region_id` | `relay` | `connection_id`、对端 `peer_id`、带宽/连接数/时长配额（以 `relay` JWT 或 TURN REST username/HMAC 承载） | 分钟级 |

每个服务**应当**至少校验：签名算法和签名、`iss`、`kid`、`aud`、`scope`、`sub`、`nbf`/`exp`；对于 `connect` JWT，还应校验 `target_peer_id`、`info_hash`、`sub_pub`（确认 `sub == SHA-256(sub_pub)` 即发起方公钥与身份一致），以及 `connection_id`（即 `jti`）的一次性消费语义。

## 7. 接口契约与运行边界

### 7.1 通用运行要求

Tracker API 基于 HTTPS REST：每个请求独立无状态，session JWT 通过 `Authorization: Bearer <jwt>` 头携带，请求/响应体使用 JSON 编码（`Content-Type: application/json`）。Tracker 不向客户端推送消息，两者之间不维护常驻信道——除 `register` 与 `revoke` 是一次性身份管理请求（不携带 session JWT）外，其余请求均携带 session JWT 按需调用。

`punch.join` / `punch.exit` / `punch.signal` 的请求响应控制面经加密认证信道传输（TCP+TLS、UDP+DTLS 或 QUIC 均可，客户端与 Punch Server 间的常驻与瞬时信令信道同此要求）；为维持 NAT 映射的 `punch.heartbeat` 可以使用 UDP，但**必须**遵循前述 MAC、时钟窗口和序列号校验。消息使用版本化 schema；未知必填字段、超出大小限制的 candidate 列表和不匹配的 `connection_id` 都**必须**拒绝。每个响应至少带协议版本、请求 ID 和明确的错误码。所有时钟校验（`nbf`/`exp`、heartbeat 的时间戳窗口）使用统一的服务端可配置容差（如 ±60 秒），客户端**应当**与可信时间源对时，容差需计入 nonce 去重窗口与序列号乱序窗口的计算。

### 7.2 接口契约表

下表定义实现必须达成的最小契约。Tracker 接口为 HTTPS REST（方法 + 路径），Punch 接口为加密信道上的 RPC（接口名 + 消息体）。

#### 7.2.1 Tracker REST API

所有请求/响应体均为 JSON（`Content-Type: application/json`）。除 `POST /peers`（register）与 `DELETE /peers/{peer_id}`（revoke）外，所有请求**必须**携带 `Authorization: Bearer <session_jwt>` 头。响应除业务字段外**应当**包含统一的错误码与请求 ID。

##### `POST /peers` — register

首次启动或密钥轮换时调用，一次性。不携带 session JWT。

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `peer_id` | string | `peer:` + base32(SHA-256(public_key)) + bech32 checksum |
| `public_key` | string | Ed25519 公钥的 base64 编码（32 字节解码后） |
| `signature` | string | `sign(private_key, "register" \|\| peer_id \|\| public_key \|\| timestamp)` 的 base64 |
| `timestamp` | int64 | 请求发起的 Unix 时间戳（秒），用于防重放 |

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `peer_id` | string | 回显 |
| `registered_at` | int64 | 入库时间戳 |
| `expires_at` | int64 | register TTL 到期时间（默认 90 天） |

**授权与限制**：验证 `peer_id == SHA-256(public_key)` 与签名；`peer_id` 未被注册或已过期才允许；同 `peer_id` 重复注册幂等返回原记录。TTL 90 天（可配置）。

**交互流程**：

```mermaid
sequenceDiagram
    participant C as client
    participant T as Tracker

    Note over C: 本地生成 Ed25519 密钥对
    Note over C: 计算 peer_id = peer:base32(sha256(pub)) + checksum
    Note over C: 签名: sign(priv, "register" || peer_id || pub || ts)
    C->>T: POST /peers {peer_id, public_key, signature, timestamp}
    Note over T: 查注册表: peer_id 未注册或已过期?
    Note over T: 验证 peer_id == SHA-256(public_key)
    Note over T: 取 public_key 验签
    Note over T: 入库: peer_id → public_key, TTL 90 天
    T-->>C: 201 {registered_at, expires_at}
    Note over C: 本地保存私钥 (Keychain/Keystore)
    Note over C: 本地生成并保存 revocation_token (见 revoke)
```

register 不需要 nonce，因为这是客户端**第一次**联系 Tracker，Tracker 对客户端一无所知，没有先验可以发 nonce。防重放靠 `timestamp` 窗口 + 同 `peer_id` 重复注册幂等返回。

##### `DELETE /peers/{peer_id}` — revoke

私钥泄露时调用，一次性。不携带 session JWT。

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `revocation_token` | string | `sign(private_key, "revoke" \|\| peer_id \|\| timestamp)` 的 base64 |

| 响应 | 说明 |
| --- | --- |
| `204 No Content` | 吊销成功 |

**授权与限制**：验证 token 签名；将 `peer_id` 加入 `revoked` 列表，TTL 7 天。被吊销 `peer_id` 的所有后续请求拒绝。

**交互流程**：

```mermaid
sequenceDiagram
    participant C as client
    participant T as Tracker

    Note over C: 私钥已泄露, 取出之前保存的 revocation_token
    C->>T: DELETE /peers/{peer_id} {revocation_token}
    Note over T: 取 public_key 验签 token
    Note over T: 加入 revoked 列表 (TTL 7 天)
    Note over T: 标记该 peer_id 所有在途 session JWT 失效
    T-->>C: 204 No Content
    Note over C,T: 攻击者即使持有旧私钥, 后续 login/join 被拒绝
```

revoke 不需要 nonce——`revocation_token` 是在 `register` 时就用私钥签好的静态声明（`sign(priv, "revoke" || peer_id || timestamp)`），一次签好后离线保存，泄露时提交即可。`timestamp` 在 token 生成时固定，Tracker 校验的是 token 签名而非时间窗口。

##### `POST /sessions` — login

会话级认证。不携带 session JWT。

挑战交互为两段式：

**第一步：获取挑战**

```
GET /sessions/challenge?peer_id=<peer_id>
```

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `nonce` | string | Tracker 下发的一次性随机数（建议 32 字节 hex） |
| `expires_at` | int64 | nonce 过期时间（建议 60 秒） |

**第二步：提交签名**

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `peer_id` | string | 客户端身份 |
| `nonce` | string | 第一步获取的 nonce |
| `signature` | string | `sign(private_key, "login" \|\| peer_id \|\| nonce \|\| timestamp)` 的 base64 |
| `timestamp` | int64 | 请求发起时间戳 |

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `session_jwt` | string | Tracker API 会话 JWT（`scope=query,announce`，`sub=peer_id`） |
| `punch_jwt` | string | 面向 Punch Server 的 punch JWT（`scope=punch`，`aud=punch_id`） |
| `punch_server` | string | 分配的 Punch Server 地址（`host:port`） |
| `stun_servers` | string[] | STUN 服务器列表（`stun:host:port`） |
| `turn_servers` | object[] | TURN 服务器列表，每项含 `url`、`region`、`expires_at` |
| `session_expires_at` | int64 | session JWT 过期时间 |

**授权与限制**：验证签名；nonce 一次性消费；`peer_id` 未被吊销；同时刷新 register TTL。

**交互流程**：

```mermaid
sequenceDiagram
    participant C as client
    participant T as Tracker

    C->>T: GET /sessions/challenge?peer_id=peer:ABCDEF...
    Note over T: 查注册表取 public_key
    Note over T: 生成 32 字节随机 nonce
    Note over T: 存内存: nonce → (peer_id, 60s 过期)
    T-->>C: {nonce, expires_at}

    Note over C: 用 private_key 签名
    Note over C: sig = sign(priv, "login" || peer_id || nonce || ts)

    C->>T: POST /sessions {peer_id, nonce, signature, timestamp}
    Note over T: 查 nonce 在内存? 属于该 peer_id? 未过期?
    Note over T: 取 public_key 验签
    Note over T: 删除 nonce (一次性消费)
    Note over T: 检查 peer_id 未被吊销
    Note over T: 签发 session JWT (scope=query,announce)
    Note over T: 签发 punch JWT (scope=punch, aud=punch_id)
    Note over T: 刷新 register TTL
    T-->>C: {session_jwt, punch_jwt, punch_server, stun_servers, turn_servers, session_expires_at}
```

**为什么两步**：Tracker 要让客户端证明"持有 peer_id 对应的私钥"，但签名内容必须包含 Tracker 下发的随机数，否则签名可被重放。第一步取随机数，第二步签随机数。攻击者即使截获完整的 `{nonce, signature}` 也无法重放——nonce 用一次就删。

**为什么 nonce 由 Tracker 生成**：如果客户端自己生成 nonce 再签，Tracker 没有记录"这个 nonce 用过"，攻击者可以重放整个 `{nonce, signature}`。Tracker 必须自己生成 nonce 并追踪是否已消费。

**为什么还要签 timestamp**：nonce 已防重放，`timestamp` 是额外保险——即使 nonce 机制出 bug，timestamp 超出窗口（±60 秒）的请求也被拒。同时 timestamp 让客户端的签名带时间戳，防止 token 被长期保存后跨场景复用。

##### `PUT /peers/{peer_id}/resources` — announce

登记或撤下自己持有的资源。携带 session JWT。

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `add` | string[] | 要登记的 `info_hash` 列表（`sha256:<hex>`） |
| `del` | string[] | 要撤下的 `info_hash` 列表 |
| `complete` | string[] | 标记为完整持有的 `info_hash` 子集（用于粗粒度候选筛选） |
| `ttl` | int32 | 期望过期时间（秒），服务端裁剪到最大值 |

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `expires_at` | int64 | 服务端裁剪后的过期时间 |

**授权与限制**：session `announce`；同一 peer/file 覆盖更新；限制 TTL 上限、批量大小、资源数和调用频率。

**交互流程**：

```mermaid
sequenceDiagram
    participant C as client
    participant T as Tracker

    C->>T: PUT /peers/{peer_id}/resources<br/>Authorization: Bearer <jwt><br/>{add: [hash1, hash2], del: [hash3], complete: [hash1], ttl: 1800}
    Note over T: 验 session JWT (scope 含 announce?)
    Note over T: sub 与 path 中的 peer_id 一致?
    Note over T: 检查批量大小/TTL 上限/频率限制
    Note over T: add 的 info_hash 入库 (peer_id → info_hash, complete, TTL)
    Note over T: del 的 info_hash 删除记录
    Note over T: 裁剪 TTL 到服务端最大值
    T-->>C: {expires_at}
    Note over C: session 期间定期重发 announce 刷新 TTL<br/>否则记录到期自动失效, peer 在查询结果中消失
```

`add` 和 `del` 在一次请求里可以同时出现——先处理 `del` 再处理 `add`，避免"先 add 再 del 同一个"导致记录丢失。`complete` 是 `add` 子集的标记，不单独维护——只对 `add` 列表里的 info_hash 生效。

##### `GET /resources/{info_hash}/peers` — query

查询持有某资源的候选 Peer。携带 session JWT。

| 请求参数 | 位置 | 说明 |
| --- | --- | --- |
| `info_hash` | path | `sha256:<hex>` 格式 |
| `limit` | query | 候选数量上限（默认 50，服务端可裁剪） |
| `cursor` | query | 分页游标（首次请求不传） |

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `peers` | object[] | 候选列表，每项见下表 |
| `next_cursor` | string? | 下一页游标，无更多数据时为 null |

每个候选 peer 对象：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `peer_id` | string | 候选身份 |
| `public_key` | string | Ed25519 公钥 base64 |
| `punch_server` | string | 该 peer 所在 Punch Server 地址 |
| `connect_jwt` | string | 一次连接所需的 connect JWT（`scope=connect`，`sub`、`sub_pub`（A 公钥）、`target_peer_id`、`info_hash`、`connection_id` 绑定） |

**授权与限制**：session `query`；限流、防枚举、不得返回 candidates 的公网地址或候选地址；候选数上限防放大。

**交互流程**：

```mermaid
sequenceDiagram
    participant A as client (A)
    participant T as Tracker

    A->>T: GET /resources/{info_hash}/peers?limit=50&cursor=...<br/>Authorization: Bearer <jwt>
    Note over T: 验 session JWT (scope 含 query?)
    Note over T: 检查限流 (按 peer_id + info_hash)
    Note over T: 查索引: info_hash → [peer_id 列表]
    Note over T: 按 limit 截取, 排除已下线/过期
    loop 对每个候选 peer
        Note over T: 生成一次性 connection_id (随机)
        Note over T: 记录 connection_id → (A, B, info_hash, ts)
        Note over T: 签发 connect JWT (scope=connect,<br/>sub=A, target_peer_id=B,<br/>info_hash, connection_id=jti, exp=数十秒)
    end
    T-->>A: {peers: [{peer_id, public_key, punch_server, connect_jwt}, ...], next_cursor}
    Note over A: 用 connect JWT 向 B 的 Punch Server 发起 signal<br/>connect JWT 过期后需重新 query
```

**为什么 Tracker 签发 connect JWT 而不是直接返回 B 的地址**：Tracker 只做发现和授权，不做转发。A 拿到 connect JWT 后直接联系 B 所在的 Punch Server，Punch B 验 JWT 后才转发信令。这样 Tracker 不在数据路径上，也不持有 A/B 的网络地址。

**为什么 connection_id = JWT 的 jti**：`jti`（JWT ID）是 JWT 规范里的一次性标识，Punch B 消费这个 jti 后就不再接受同一 jti 的请求——天然防重放。同时 Tracker 记录 `connection_id → (A, B, info_hash)` 作为后续 `relay_credentials` 的授权依据。

##### `POST /connections/{connection_id}/relay` — relay_credentials

请求 TURN 中继凭据。携带 session JWT。

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `connection_id` | string | 来自 connect JWT 的 `jti` |

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `turn_username` | string | 短期 TURN REST 用户名 |
| `turn_password` | string | 短期 TURN REST 密码（HMAC） |
| `turn_servers` | string[] | TURN 服务器列表 |
| `expires_at` | int64 | 凭据过期时间 |

**授权与限制**：仅连接双方；按签发记录校验 `sub` 与 `connection_id` 记录，并检查配额。

**交互流程**：

```mermaid
sequenceDiagram
    participant A as client (A)
    participant T as Tracker

    Note over A: 直连 ICE 失败, 决定走 TURN
    A->>T: POST /connections/{connection_id}/relay<br/>Authorization: Bearer <jwt>
    Note over T: 验 session JWT
    Note over T: 查 connection_id 记录: sub == A? 对端 == B? 记录过期?
    Note over T: 检查 TURN 配额 (带宽/连接数/时长)
    Note over T: 生成短期 TURN REST 凭据:<br/>username = "expiry_timestamp:peer_id"<br/>password = HMAC(turn_secret, username)
    Note over T: 记录凭据发放 (计费/审计)
    T-->>A: {turn_username, turn_password, turn_servers, expires_at}
    Note over A: 用凭据连接 TURN 服务器<br/>B 也会独立请求自己的 relay_credentials<br/>TURN 凭据过期后需重新请求
```

**为什么 A 和 B 各自请求**：TURN REST 凭据是 per-peer 的——用户名里编码了 peer_id，TURN 服务器按 peer_id 计费和限流。A 和 B 拿到的是各自的凭据，不是共享的。

**为什么 Tracker 记录 connection_id**：`relay_credentials` 的授权依据是 Tracker 在 `query` 时签发的 `connection_id → (A, B, info_hash)` 记录。只有该记录里的双方才能请求 relay 凭据，防止第三方滥用。

#### 7.2.2 Punch RPC 接口

Punch Server 上的接口走加密认证信道（非 REST），以 RPC 风格的接口名 + 消息体交互。

##### `punch.join`

建立 Binding。

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `punch_jwt` | string | 来自 login 的 punch JWT（`scope=punch`） |
| `nonce` | string | Punch Server 下发的一次性随机数 |
| `punch_id` | string | punch JWT 的 `aud` |
| `signature` | string | `sign(private_key, "join" \|\| nonce \|\| punch_id \|\| timestamp)` 的 base64 |
| `timestamp` | int64 | 请求时间戳 |

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `binding_id` | string | Binding 唯一标识 |
| `binding_key` | string | 会话级对称密钥（HMAC 用） |
| `expires_at` | int64 | Binding 过期时间 |

**授权与限制**：JWT + 持钥证明；nonce 一次性消费；`peer_id` 未被吊销；新 binding 替换旧 binding。

**交互流程**：

```mermaid
sequenceDiagram
    participant B as client (B)
    participant PB as Punch Server

    Note over B: 已通过 login 获得 punch JWT
    Note over B: 已建立到 PB 的加密信道

    B->>PB: punch.challenge(peer_id)
    Note over PB: 生成 nonce
    Note over PB: 存内存: nonce → (peer_id, 短过期)
    PB-->>B: {nonce, punch_id, expires_at}

    Note over B: 签名: sign(priv, "join" || nonce || punch_id || ts)

    B->>PB: punch.join(punch_jwt, nonce, punch_id, signature, ts)
    Note over PB: 验 punch JWT (签名, aud, scope, exp)<br/>JWT 的 sub == peer_id?
    Note over PB: 查 nonce 有效且属于该 peer_id
    Note over PB: 查 peer_id 未被吊销 (查 Tracker 或缓存)
    Note over PB: 取 public_key 验签
    Note over PB: 删除 nonce (一次性)
    Note over PB: 生成 binding_id (随机)
    Note over PB: 生成 binding_key (随机 32 字节对称密钥)
    Note over PB: 旧 binding (如有) 立即失效
    Note over PB: 入库: binding_id → (peer_id, binding_key, TTL)
    PB-->>B: {binding_id, binding_key, expires_at}

    Note over B: 本地保存 binding_id + binding_key<br/>后续 heartbeat/leave 用 binding_key 做 HMAC, 不再签名
```

`punch.challenge` 是 `punch.join` 的前置步骤，获取 Punch Server 下发的 nonce。它与 login 的 challenge 机制同构——Punch Server 对客户端零先验，必须用随机数防重放。

**为什么 join 也要签名**：Punch Server 是独立服务，对客户端**零先验**。punch JWT 证明 Tracker 授权了这次接入，但 JWT 可能被偷。签名证明"持有 peer_id 对应的私钥"，把"JWT 持有者"和"私钥持有者"绑定。

**为什么 join 后改用 HMAC**：join 时已经用非对称签名建立了 binding_key，后续 heartbeat 每隔几秒发一次（维持 NAT 映射），用对称 HMAC 比 Ed25519 签名快 10-50 倍，UDP 包也更小。binding_key 只在加密信道内传输，泄露面小。

##### `punch.heartbeat`

刷新 Binding 与 NAT 映射。

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `binding_id` | string | Binding 标识 |
| `seq` | uint64 | 递增序列号 |
| `timestamp` | int64 | 请求时间戳 |
| `mac` | string | `HMAC-SHA-256(binding_key, binding_id \|\| seq \|\| timestamp \|\| transport_generation)` 的 hex |

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `expires_at` | int64 | 刷新后的过期时间 |

**授权与限制**：有效 binding；校验 MAC、时钟窗口（±60s）和序列窗口（允许有限乱序）。

**交互流程**：

```mermaid
sequenceDiagram
    participant C as client
    participant P as Punch Server

    Note over C: 每隔 N 秒, 维持 NAT 映射
    Note over C: seq = 上次 seq + 1
    Note over C: mac = HMAC(binding_key,<br/>binding_id || seq || timestamp || generation)

    C->>P: heartbeat(binding_id, seq, timestamp, mac)<br/>(走 UDP, 不需加密信道)
    Note over P: 查 binding_id 有效?
    Note over P: 校验 mac: 重算 HMAC(binding_key, ...), 比对
    Note over P: 校验 timestamp 在 ±60s 窗口内
    Note over P: 校验 seq > 上次接受的 seq (允许有限乱序窗口, 如 ±5)
    Note over P: 刷新 binding TTL
    P-->>C: {expires_at}
```

**为什么用 HMAC 不用 JWT**：heartbeat 每隔几秒发一次（保活 + 刷新 NAT），用 JWT 每次验签成本高。HMAC 是对称运算，快 10-50 倍。binding_key 在 join 时通过加密信道下发，只有客户端和 Punch Server 持有。

**为什么走 UDP**：heartbeat 的目的是刷新 NAT 映射（让路由器保持端口转发），UDP 天然适合——不需要 TCP 的握手开销。UDP 不加密但 MAC 已防篡改和防重放。

**为什么有 transport_generation**：ICE restart（网络切换）时 candidate 地址会变，generation 递增。MAC 覆盖 generation，防止旧 generation 的 heartbeat 被用来维持已失效的 NAT 映射。

##### `punch.exit`

主动下线。

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `binding_id` | string | Binding 标识 |
| `seq` | uint64 | 序列号 |
| `mac` | string | 同 heartbeat 的 MAC 算法 |

**授权与限制**：有效 binding；校验 MAC 与序列号；成功后 binding 立即失效。

**交互流程**：

```mermaid
sequenceDiagram
    participant C as client
    participant P as Punch Server

    Note over C: 客户端主动下线
    Note over C: seq = 当前 seq + 1
    Note over C: mac = HMAC(binding_key,<br/>binding_id || seq || timestamp || generation)

    C->>P: exit(binding_id, seq, mac)
    Note over P: 查 binding_id 有效?
    Note over P: 校验 mac + seq
    Note over P: 删除 binding 记录
    Note over P: binding_key 立即作废
    P-->>C: 204 No Content
    Note over C,P: 此后该 peer 的信令信道关闭<br/>其他 peer 无法再通过 PB 联系到它
```

**为什么 exit 复用 heartbeat 的 MAC 机制**：客户端已经持有 binding_key，不需要新凭据。exit 和 heartbeat 用同样的 MAC 算法，Punch Server 用同样的校验逻辑，代码路径统一。

**为什么不先 announce(del) 再 exit**：客户端下线时**应当**先通过 `announce` 的 `del` 撤下自己的全部资源（让 Tracker 停止把该 peer 作为查询候选返回），再 `exit` Punch Server。如果只 exit 不 del，Tracker 的资源索引要等 TTL 过期才消失，期间其他 peer query 到该 peer 后发起连接会失败。

##### `punch.signal`

信令交换。`type` 决定消息体的字段集。

| 请求字段 | 类型 | 说明 |
| --- | --- | --- |
| `type` | enum | `offer` / `answer` / `candidate` / `cancel` |
| `connection_id` | string | 来自 connect JWT 的 `jti` |
| `connect_jwt` | string? | `type=offer` 时**必须**携带；其余类型不重复验证 |
| `sdp` | string? | `type=offer` / `answer` 时携带 SDP |
| `candidate` | string? | `type=candidate` 时携带 ICE candidate 字符串 |
| `signal_key` | string? | 断线重连时携带，用于恢复信令信道 |

| 响应字段 | 类型 | 说明 |
| --- | --- | --- |
| `signal_key` | string? | **仅 `type=offer` 的首个响应**携带，后续消息不重复 |
| `ok` | bool | 转发确认 |
| `error` | string? | 错误码（如 `peer_offline`、`rate_limited`、`invalid_jwt`） |

**授权与限制**：offer 验证 connect JWT（`aud`、`scope`、`sub`、`sub_pub`、`target_peer_id`、`info_hash`、`connection_id`、`exp`）；answer/candidate/cancel 仅接受已登记连接的双方；SDP 对 Punch Server 不透明；A 断线重连凭 `connection_id` + `signal_key` 恢复。

**交互流程（offer — A 发起连接）**：

```mermaid
sequenceDiagram
    participant A as client (A)
    participant PB as Punch B
    participant B as client (B)

    Note over A: 已通过 query 获得对 B 的 connect JWT<br/>(JWT claims 含 sub=A peer_id, sub_pub=A 公钥)
    Note over A: 本地已收集 ICE 候选

    A->>PB: signal(type=offer, connection_id, connect_jwt,<br/>sdp=A 的 SDP offer + DTLS 指纹 + ICE 候选)
    Note over PB: 验 connect JWT: 签名, aud, scope, sub,<br/>target_peer_id, info_hash, connection_id, exp
    Note over PB: 从 JWT 取 sub_pub (A 公钥), 校验 sub == SHA-256(sub_pub)
    Note over PB: 原子登记 connection_id (一次性消费 jti)
    Note over PB: 生成 signal_key (随机)
    Note over PB: 存连接记录: connection_id → (A, B, info_hash, signal_key, ...)
    PB-->>A: {signal_key, ok}

    PB->>B: signal(type=offer, connection_id, info_hash,<br/>A peer_id, connect_jwt, sdp)
    Note over B: 验签 connect JWT (用 Tracker 公钥)
    Note over B: 从 JWT 取 sub_pub (A 公钥), 校验 sub == SHA-256(sub_pub)
    Note over B: 校验 target_peer_id == self
    Note over B: 决定接受/拒绝
```

**Punch B 怎么知道 A 的公钥**：A 的公钥由 Tracker 签发 connect JWT 时放入 `sub_pub` claim。Punch B 验 JWT 后从 payload 取出 `sub_pub`，校验 `sub == SHA-256(sub_pub)` 确认公钥与身份一致，再转发给 B。B 同样从 JWT 取 `sub_pub` 并独立校验。这样 A 不需要在 signal 请求里额外携带公钥——公钥的权威性由 Tracker 签名保证。

**交互流程（answer — B 应答）**：

```mermaid
sequenceDiagram
    participant A as client (A)
    participant PB as Punch B
    participant B as client (B)

    B->>PB: signal(type=answer, connection_id,<br/>sdp=B 的 SDP answer + DTLS 指纹 + ICE 候选)
    Note over PB: 检查连接记录仍有效?
    Note over PB: 检查 B 仍处于有效 binding?
    PB-->>B: {ok}
    PB->>A: signal(type=answer, connection_id, B peer_id, sdp)
    Note over A: 验证 B 的 DTLS 证书指纹<br/>== SDP 中的指纹<br/>== connect JWT 对应公钥的派生
```

**交互流程（candidate — trickle ICE）**：

```mermaid
sequenceDiagram
    participant S as A 或 B
    participant PB as Punch B
    participant O as 对端

    Note over S: ICE agent 发现新候选地址
    S->>PB: signal(type=candidate, connection_id,<br/>candidate="host:...")
    Note over PB: 检查连接记录有效?
    PB-->>S: {ok}
    PB->>O: signal(type=candidate, connection_id, candidate)
```

**交互流程（cancel — 取消连接）**：

```mermaid
sequenceDiagram
    participant S as A 或 B
    participant PB as Punch B
    participant O as 对端

    Note over S: 决定取消连接
    S->>PB: signal(type=cancel, connection_id)
    Note over PB: 删除连接记录
    Note over PB: 停止转发
    PB-->>S: {ok}
    PB->>O: signal(type=cancel, connection_id)
    Note over O: 收到后立即清理本地状态
```

**交互流程（断线重连）**：

```mermaid
sequenceDiagram
    participant A as client (A)
    participant PB as Punch B

    Note over A: 瞬时信道断开
    Note over A: 仍有 connection_id 和 signal_key
    Note over A: 建立新加密信道
    A->>PB: signal(type=offer, connection_id, signal_key)
    Note over PB: 查连接记录: connection_id 仍有效?
    Note over PB: 校验 signal_key 匹配?
    Note over PB: 恢复 A 的信道绑定
    PB-->>A: 补发未送达的信令 (通常是 answer 和少量 candidate)
    Note over PB: 按序补发, 送达或超时后丢弃
```

**为什么 offer 要带 connect JWT 而 answer 不用**：offer 是连接的**第一次**消息，Punch B 必须验证 A 有权发起连接（connect JWT 由 Tracker 签发，绑定 A/B/info_hash/connection_id）。一旦 connection_id 被原子登记（jti 一次性消费），后续 answer/candidate/cancel 只需凭 `connection_id` 匹配已登记的连接——信道即凭据。

**为什么 signal_key 只在 offer 响应里下发一次**：signal_key 是对称密钥，用于断线重连时恢复信道。它在加密信道内传输，不暴露给网络。后续消息不再重复发送 signal_key，减少泄露面。

### 7.3 客户端函数面

#### 7.3.1 Tracker 通信接口

客户端对 Tracker 的通信面只有**请求**与**响应**两类，没有 Relay——Tracker 不向客户端推送消息，两者之间也不维护常驻信道；`register` 与 `revoke` 是一次性的身份管理请求，不携带 session JWT；`login` 是会话级认证请求；其余请求均携带 session JWT，在有效期内按需调用。接口与 7.2.1 节的 REST API 一一对应。

| 函数 | 方向 | 对应 REST | 说明 |
| --- | --- | --- | --- |
| `SendRegisterReq` / `OnRegisterRsp` | client ↔ Tracker | `POST /peers` | 首次启动或密钥轮换时调用，一次性 |
| `SendRevokeReq` / `OnRevokeRsp` | client → Tracker | `DELETE /peers/{peer_id}` | 私钥泄露时提交 revocation token，一次性 |
| `SendLoginChallengeReq` / `OnLoginChallengeRsp` | client ↔ Tracker | `GET /sessions/challenge` | 获取登录挑战 nonce |
| `SendLoginReq` / `OnLoginRsp` | client ↔ Tracker | `POST /sessions` | 提交签名，rsp 含 session JWT、punch JWT、Punch Server 分配与 STUN/TURN 配置 |
| `SendAnnounceReq` / `OnAnnounceRsp` | client ↔ Tracker | `PUT /peers/{peer_id}/resources` | `add` / `del` 批量登记与撤下资源 |
| `SendQueryReq` / `OnQueryRsp` | client ↔ Tracker | `GET /resources/{info_hash}/peers` | rsp 含候选 Peer 的 `peer_id`、公钥、Punch 地址与 connect JWT |
| `SendRelayCredentialsReq` / `OnRelayCredentialsRsp` | client ↔ Tracker | `POST /connections/{connection_id}/relay` | rsp 仅含请求方自身的 relay 凭据 |

#### 7.3.2 Punch 通信接口

客户端对 Punch Server 的通信面分为三类：**请求**（客户端发起，必有配对响应）、**响应**（对某个在途请求的确认或错误）、**Relay**（Punch Server 转发来的对端信令，客户端没有对应在途请求）。A / B 是运行时角色，不是两套接口——同一个 client 既维护到自己 Punch Server 的常驻信道（对应 B 角色），也在发起连接时临时连接目标 Punch Server（对应 A 角色）。表中以 A（发起方）、B（提供方）、PA / PB（各自与目标的 Punch Server）标注方向。

| 函数 | 方向 | 说明 |
| --- | --- | --- |
| `SendJoinReq` / `OnJoinRsp` | A ↔ PA；B ↔ PB | `punch.join`；rsp 含 `binding_id`、`binding_key` |
| `SendExitReq` / `OnExitRsp` | A ↔ PA；B ↔ PB | `punch.exit` |
| `SendHeartbeatReq` / `OnHeartbeatRsp` | A ↔ PA；B ↔ PB | `punch.heartbeat`；rsp 含刷新后的过期时间 |
| `SendSignalReq(offer)` | A → PB | 首个请求建立瞬时信令信道；承载 SDP offer；rsp 含 `signal_key` |
| `SendSignalReq(answer)` | B → PB | 走 B 的常驻信道；承载 SDP answer |
| `SendSignalReq(candidate)` | A 或 B → PB | trickle ICE 候选（含 relay 回退阶段） |
| `SendSignalReq(cancel)` | A 或 B → PB | 对端会收到 Relay 的 `cancel` |
| `OnSignalRsp(type)` | PB → A 或 B | 任何 `signal` 请求的确认或错误（如转发成功、记录失效、对端不可达） |
| `OnSignalRelay(offer)` | PB → B | 转发的 offer，走 B 的常驻信道 |
| `OnSignalRelay(answer)` | PB → A | 转发的 answer，走 A 的瞬时信道 |
| `OnSignalRelay(candidate)` | PB → A 或 B | 转发的对端候选地址 |
| `OnSignalRelay(cancel)` | PB → A 或 B | 非发送方收到后立即清理本地状态 |

断线重连（`connection_id` + `signal_key`）属于信道层，不占业务函数；重连后补发的未送达信令同样从 `OnSignalRelay` 进入。`Relay` 在此指"经 Punch Server 转发的信令"，与 TURN / Relay Server 无关。

#### 7.3.3 Peer 通信接口

Peer 之间的消息是对称、单向的，没有控制面那种请求-响应对，因此函数不带 Req / Rsp 后缀。`bitfield`、`have`、`request`、`reject`、`cancel`、`request_info`、`info` 走控制通道（reliable + ordered），`piece` 走数据通道（unreliable + unordered，`maxRetransmits = 0`）；连接建立与断线重连属信道层（ICE / DTLS），不占业务函数。

| 函数 | 方向 | 说明 |
| --- | --- | --- |
| `SendBitfield` / `OnBitfield` | A ↔ B | 连接建立后一次性交换；为空可跳过 |
| `SendRequestInfo` / `OnRequestInfo` | 缺 info 方 → 对端 | magnet 模式：bitfield 交换前发起 |
| `SendInfo` / `OnInfo` | 对端 → 请求方 | info 字节串；接收方校验 `SHA-256 == info_hash` 后使用 |
| `SendHave` / `OnHave` | A ↔ B | 每验证一个 block 广播给所有已连接 Peer |
| `SendRequest` / `OnRequest` | 下载方 → 上传方 | 在途窗口内；响应为 `piece` 或 `reject` |
| `SendPiece` / `OnPiece` | 上传方 → 下载方 | 数据本体，走数据通道 |
| `SendReject` / `OnReject` | 上传方 → 下载方 | 按单个请求粒度拒绝；下载方退避或转投 |
| `SendCancel` / `OnCancel` | 下载方 → 上传方 | 撤销在途请求（endgame / 转投） |

## 8. 安全性考量

### 8.1 控制面

- 没有面向 Punch 的 `punch JWT`：攻击者可伪造任意 `peer_id` 绑定到 Punch Server，造成身份冒充、在线表污染或会话劫持。
- 没有 `heartbeat` 的 MAC、序列号和过期管理：攻击者可伪造或重放保活包，使虚假 Binding 长期存在，或干扰真实 Peer 的可达性。
- 没有针对 B 的短期 `connect` JWT：任何人只要知道 Punch B 地址，就能要求它向 B 投递信令；Punch Server 会被用于骚扰、扫描、反射流量或对大量 Peer 发起连接请求。
- 只靠 `scope=connect`、却不校验 `aud` 与 `target_peer_id`：一张泄露票据可被拿去联系其他 Punch 节点或其他 Peer，权限范围过大。
- 没有 `exp`、`jti`：截获的连接请求可在很久以后无限重放。
- 没有独立的 TURN 授权与配额：攻击者可把 TURN 当开放代理消耗带宽，造成高额成本与滥用风险。
- 没有密钥吊销机制：私钥泄露后，攻击者可在旧 session JWT 过期前持续冒充身份（最长数小时）。本设计通过 revocation token（第 3.1.4 节）解决，但要求客户端妥善备份 token；token 丢失时只能等 register TTL（默认 90 天）过期，期间身份仍可被冒充。

### 8.2 数据面

- 没有端到端身份认证和加密：即便 Punch/TURN 已正确鉴权，仍无法阻止恶意中继或网络路径窥探、篡改、冒充对端。
- 恶意 Peer 投递损坏数据：block 哈希校验使污染数据无法进入 verified 状态，攻击者最多浪费下载方带宽，并累积失败计数直至被断开与拒绝重连。
- 伪造 info 或谎报 bitfield：`SHA-256(info) == info_hash` 自校验使伪造 info 无法通过；谎报 bitfield 的 Peer 要么交不出数据计入失败，要么伪造数据过不了 block 哈希。
- CDN 内容不可信：CDN 作为虚拟 Peer 的数据同样经 info 哈希校验，CDN 被入侵或内容被篡改不产生额外信任面。
- 恶意巨型种子文件：解析前检查总长上限（第 5.2 节），防御 DoS。

## 9. 参考资料

以下参考均为 informative（参考性），本设计与其中协议不互通。

- RFC 2119 / RFC 8174：规范性关键词。
- RFC 8445：ICE（Interactive Connectivity Establishment）。
- RFC 5766：TURN（Traversal Using Relays around NAT）。
- RFC 7675：ICE 连接保活与 consent。
- RFC 8831 / RFC 8832：WebRTC Data Channels 与 DCEP。
- RFC 8785：JSON Canonicalization Scheme（本设计比较后未采用）。
- BEP 3（BitTorrent 协议）、BEP 6（fast extension）、BEP 19（web seeding）、BEP 52（v2）：消息语义与元数据形态的主要参考。
- coturn TURN REST API（static-auth-secret 短期凭据）。
