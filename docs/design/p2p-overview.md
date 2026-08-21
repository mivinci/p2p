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

**`peer_id`**：由客户端长期保存的 Ed25519 公钥派生（`SHA-256(public_key)`），32 字节，由客户端本地生成、不由 Tracker 分配——密钥即身份，Tracker 只是发现与授权的控制面（密钥轮换即更换身份，见第 3.1 节）。机器间协议始终使用二进制 `peer_id`。

**`peer_fingerprint`**：`peer_id` 的可读编码，用于客户端 UI 显示、用户肉眼确认与手动输入场景。格式为 `p2p:` + base32(`peer_id` 前 12 字节) + bech32 校验位，分 5 组以连字符分隔，共 24 字符，例如 `p2p:ABCD2345-EFGH6789-JKL0-XXXX`。base32 字母表为 RFC 4648（不含 `0/O/1/I` 以避免视觉混淆），bech32 校验位（BCH 码）可检测 4 位以内的转录错误。fingerprint 仅作人机界面层编码，**不得**出现在协议消息中；两台设备上 fingerprint 一致即代表同一身份。

**Ed25519**：本设计全程使用 Ed25519 作为签名算法。选择理由：公钥仅 32 字节、签名 64 字节、验签速度比 RSA-2048 快 10–50 倍（对每次 `enter` 都要签名的移动端场景关键）、抗侧信道攻击。本设计**不**使用 RSA、ECDSA 或 GPG/OpenPGP 密钥格式——客户端自己生成并保存 Ed25519 密钥对，不依赖外部密钥管理工具。

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
        T-->>A: session JWT；enter JWT（aud=Punch A，scope=enter）；STUN/TURN 配置
        A->>PA: enter(enter JWT, proof_of_possession)
        PA-->>A: binding_id、binding_key、expires_at
        A->>STUN: Binding Request
        STUN-->>A: A candidates（host / srflx）
    and B 上线并登记资源
        B->>T: register(peer_id, public_key, signature)  [首次]
        T-->>B: 注册确认
        B->>T: login(peer_id, nonce_signature)
        T-->>B: session JWT；enter JWT（aud=Punch B，scope=enter）；STUN/TURN 配置
        B->>PB: enter(enter JWT, proof_of_possession)
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
    PB->>B: signal(type=offer, connection_id, info_hash, A peer_id、公钥、SDP offer + A candidates)
    B->>PB: signal(type=answer, connection_id, SDP answer + B candidates)
    PB->>PB: 检查 B 仍处于有效 Binding
    PB-->>A: signal(type=answer, connection_id, B peer_id, SDP answer + B candidates)

    par ICE 连通性检查
        A->>B: ICE checks + DTLS 握手
    and
        B->>A: ICE checks + DTLS 握手
    end
    Note over A,B: 身份锚定：对端 DTLS 证书指纹与信令中 SDP 一致；connect JWT 的 sub 与 offer 中公钥派生关系一致

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

Tracker 验签后，将 `peer_id` 加入 `revoked` 列表（带吊销时间戳）。此后任何针对该 `peer_id` 的 `login` / `enter` 请求**必须**拒绝；Punch Server 在 `enter` 时**应当**通过 Tracker 的公开 API 校验 `peer_id` 是否被吊销（可短时间缓存以降低延迟），或依赖 session JWT 上的吊销标记。

`revoked` 列表带 TTL（**应当**为 7 天，与 register TTL 解耦）。TTL 到期后 `peer_id` 可被重新注册——这平衡了"吊销有效性"与"peer_id 永久占用"。Token 丢失时只能等 register TTL 过期后重新注册（期间身份仍可被冒充，因此客户端**应当**妥善备份 token）。

#### 3.1.5 客户端 UI 中的 fingerprint

客户端 UI **应当**在"我的身份"页面同时显示：

- `peer_fingerprint`（24 字符，分 5 组）：用于肉眼快速识别、添加好友、口耳相传确认
- 完整 `peer_id`（32 字节 hex，折叠显示）：用于高级场景的诊断与日志检索
- 二维码：编码 `peer_fingerprint`，方便线下扫码添加

用户在两台设备上确认 fingerprint 一致即可确认是同一身份，无需比对完整 `peer_id`。**不得**在协议消息、日志或 URL 中使用 fingerprint——机器间始终用二进制 `peer_id`，fingerprint 仅在客户端 UI 层编码与解码。

### 3.2 Binding：`enter`、`heartbeat` 与 `leave`

客户端使用仅面向自身 Punch Server 的 `enter` JWT 建立 Binding，并使用 `peer_id` 对应私钥对本次请求的 nonce、`punch_id`、时间戳签名，证明令牌持有者确实控制该身份。单独的 bearer JWT **不得**作为 enter 的唯一凭据；nonce **必须**一次性消费（Punch Server 维护覆盖时间窗口两倍时长的去重记录），时间戳超出短窗口的请求**必须**拒绝，否则被截获的 enter 请求在 JWT 有效期内可被重放。Punch Server 验签并验证该证明后创建 `binding_id`，并通过该加密信道的已认证响应返回随机生成的会话级 `binding_key`。此 Binding 代表"该 peer 当前可通过这个 Punch Server 被联系到"。

之后的保活使用 `heartbeat(binding_id, seq, MAC)`，而不是每次发送完整 JWT。MAC 固定为 `HMAC-SHA-256(binding_key, binding_id || seq || timestamp || transport_generation)`；服务端仅接受递增序列号（允许一个有限乱序窗口）且 timestamp 在短窗口内的请求。Binding 到期、客户端重新 enter、或网络切换时，旧 `binding_key` 和旧序列号空间立即失效。这样既刷新 NAT 映射和在线 TTL，也避免频繁验签与较大的 UDP 包。

主动下线使用 `leave(binding_id, seq, MAC)`，复用 heartbeat 的 MAC 与序列号机制，无需新凭据；成功后 Binding 立即删除、`binding_key` 作废。客户端下线时**应当**先通过 `announce` 的 `del` 撤下自己的全部资源。Tracker 不提供 session 吊销：JWT 无状态，短 `exp` 已把泄露损失框在有限时间内，"下线"的语义由 `leave` 与资源撤下共同承担。

### 3.3 ICE 候选收集

候选地址由 ICE agent 收集：host candidate（局域网地址，**应当**以 mDNS 名称暴露）与 srflx candidate（经 STUN 映射的公网地址）。连通性检查、角色仲裁（controlling / controlled）、candidate 配对与 consent 保活均由 ICE 完成，客户端无需自行实现同时探测；网络切换通过 ICE restart 触发新一轮收集与检查。

Tracker 不保存或下发这些地址：候选地址会随网络切换而变化，且提前暴露会扩大隐私泄露与扫描风险。候选地址只在获得连接授权后的 Punch 信令中交换（trickle ICE）。candidate 使用标准 ICE candidate 编码，携带优先级与 `transport_generation`（ICE restart 时递增）；generation 变化后旧 candidate **不得**使用。默认不对非同一私网的 Peer 发送 host candidate，以避免泄露内网拓扑；允许发送时**应当**使用 mDNS 名称或由用户显式选择。

### 3.4 资源宣告与查询

B 使用 `announce` 登记自己可提供的资源：可一次 `add` 一组 `info_hash`（各带 `complete` 标志、TTL），或一次 `del` 一组 `info_hash` 主动撤下索引（不必等 TTL 过期）；记录**必须**带 TTL（服务端设置最大值），离线后自动失效。A 使用 `query(info_hash)` 查询少量候选 Peer。`info_hash` 固定为 info 的 SHA-256 摘要，info 的编码与字段见第 5 节。`complete` 标记是否持有整个文件，供 query 做粗粒度候选筛选（避开与自己没有交集的 Peer，降低 connect JWT 的无效消耗）；块级信息不上报 Tracker——Tracker 只维护 peer_id → info_hash 映射与 `complete` 标志，不保存全局块位图，细粒度的块可见性与稀缺度属于数据面，由直连双方交换维护。**不得**使用调用方自定义的文件名或非规范哈希作为资源标识。

Tracker 的查询结果仅包含 B 的 `peer_id`、B 的身份公钥、B 所在 Punch Server 地址和一次连接所需的 `connect` JWT；**不包含** B 的公网地址或候选地址。`connect` JWT 以自身的一次性 `jti` 作为 `connection_id`（由 Tracker 生成、不可预测）；Tracker 在签发时记录 `connection_id → (A, B, info_hash, 创建时间)`，TTL 为连接建立超时，作为后续 `relay_credentials` 的授权依据。**应当**限制候选数量、对 query/announce 按身份和资源施加速率限制，并限制单个资源的返回分页，以避免热门资源的索引放大和完整 Peer 列表泄露。

### 3.5 Punch 信令交换

A 使用 `connect` JWT 中的 `connection_id`（即该 JWT 的一次性 `jti`，由 Tracker 生成、不可预测），并通过 `punch.signal` 发送 `type=offer`，其中包含自己的 SDP offer（含 DTLS 证书指纹与 ICE 用户名/口令）、候选地址与 `connect` JWT。Punch B 验证 `aud`、`scope`、`sub`、`target_peer_id`、`info_hash`、`connection_id`、`exp` 后，原子登记该 `connection_id`（即消费掉这个一次性 `jti`），并在该请求的响应中下发 `signal_key`（见下文），再向 B 转发该 offer（含 `info_hash`、`connection_id` 与 connect JWT 本体），使 B 能在应答前基于文件决定接受或拒绝，并能独立验签 JWT、校验 `target_peer_id` 与 offer 中 A 公钥的派生关系。B 再通过同一个 `punch.signal` 接口发送 `type=answer`，携带 SDP answer（含 B 的 DTLS 证书指纹）、最新候选与 `connection_id`；Punch B 仅在该记录仍有效且 B 处于有效 Binding 时将其转发给 A。

同一 `connection_id` 的连接记录有效期内，相同 offer 的重传**必须**幂等，并返回已有处理结果；不同内容复用同一 `connection_id` **必须**拒绝。若在连接建立超时前未收到 answer，A **应当**重新 `query` 获取新的 `connect` JWT 与新的 `connection_id` 发起新连接。`cancel` 会使 Punch B 删除该连接记录、停止转发，并向对端转发一条 `type=cancel`，对端收到后立即清理本地状态；未收到通知的一端仍**应当**在固定连接超时后清理记录。离线、令牌过期、目标拒绝、限流**应当**返回可区分的错误码，但对未授权请求不泄露目标是否在线。

`offer`、`answer`、`candidate` 和 `cancel` 都是 `punch.signal` 的消息类型，而不是独立接口。`offer` / `answer` 承载 SDP，Punch Server 将其视为不透明数据中继，**不得**解析；`candidate` 承载标准 ICE candidate 字符串（trickle ICE），均使用同一个 `connection_id`。SDP 中的 DTLS 证书指纹经 Punch 认证信道送达，构成端到端身份校验的锚点（见第 4.1 节）。

因此，A 直接请求 **Punch B**，而不是先经由 Punch A 再转发。Punch A 的职责仅是维护 A 自己的在线 Binding；只有当其他 Peer 主动连接 A 时，Punch A 才进入信令路径——本流程中 A 是发起方，Punch A 不出现。Punch B 则因为持有 B 的有效 Binding，能够把信令送达 B。

A 一侧的信令走**连接范围的瞬时加密信道**：随 A 的首个 `signal`（offer）建立，生命周期与 Punch B 上的连接记录一致（连接建立成功或超时即关闭），answer、trickle candidate、cancel 与错误都经它双向传递。首个 `signal` 以 connect JWT 认证；此后同一信道上的消息不再重复验证 JWT——信道即凭据，relay 回退阶段的 candidate 交换因此不受 JWT `exp` 限制。Punch B 在首个 `signal` 的响应中同时下发随机生成的 `signal_key`（对称密钥，只在加密信道内出现，有效期与连接记录一致）；这与 Binding 的模式同构：enter JWT 之于 `binding_key`，正如 connect JWT 之于 `signal_key`。若瞬时信道意外断开，A 重连 Punch B 并出示 `connection_id` 与 `signal_key` 即可恢复该连接的信令；Punch B 在连接记录中保留已收到但未送达 A 的信令（通常是 answer 与少量 candidate），重连后按序补发，送达或超时后丢弃。

B 一侧的信令送达依赖 B 与 Punch B 之间在 enter 时建立的常驻加密信道：heartbeat 同时为该信道保活；信道断开即视为 Binding 失效，Punch Server 对后续连接请求返回可区分的"目标不可达"错误。两类信道的具体传输均不作限定，TCP+TLS、UDP+DTLS 或 QUIC 均可，硬性要求相同——机密性与完整性（`binding_key`、`signal_key` 与 connect JWT 均只经此类信道传输）、服务器认证（防止假 Punch Server 接管信令面）、以及网络切换后的可恢复性（常驻信道凭重新 enter 与 `transport_generation` 语义恢复，瞬时信道凭 `signal_key` 重连）。

这也是部署前提：Punch Server 的容量需按两类连接估算——面向 B 的常驻 Binding 数（小时级），以及面向 A 的挂起瞬时信道数（数十秒级，受 connect JWT 有效期与连接超时共同框定），并对单个 peer/IP 的并发挂起信道数设上限，防止批量挂连接耗尽资源。

## 4. 数据面协议

本章描述连接两端 Peer 之间的交互：ICE 连通性与端到端认证、数据组织（block 与 piece）、info 拉取（magnet 模式）、块可见性交换（bitfield 与 have）、piece 级请求与传输、block 选择与上传调度，以及直连失败时的 TURN 回退。

### 4.1 ICE 连通性与端到端认证

拿到彼此 SDP 与候选地址后，ICE agent 自动完成连通性检查（同时向对方候选发包建立 NAT 映射、角色仲裁、提名），DTLS 在选中的路径上完成加密握手并建立 SCTP 关联。打洞与握手本身无需自行设计，需要设计的是**身份锚定**：

- offer / answer 中的 DTLS 证书指纹经 Punch 认证信道送达（A 侧凭 connect JWT 建立的瞬时信道，B 侧凭 Binding 常驻信道）；连接建立时双方**必须**校验对端实际 DTLS 证书与信令中指纹一致，防止信令之后的路径替换。
- B 侧**必须**校验 Punch 转发的 offer 所附 connect JWT（Tracker 验签、`target_peer_id` 为自己、`info_hash` 一致），并确认 offer 中的 A 公钥能派生出 JWT 的 `sub`；A 侧确认 answer 经 B 的有效 Binding 送达，且 Tracker 在 query 结果中返回的 B 公钥能派生出目标 `peer_id`。

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
  tracker_list: ["wss://tracker1.example.com",
                 "wss://tracker2.example.com"]   Tracker 地址列表，不参与哈希；可空（依赖客户端配置）
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
| 建立/刷新 Punch Binding | 指定 `punch_id` | `enter` | `sub` | 10–30 分钟 |
| 请求连接目标 Peer | 指定 `punch_b_id` | `connect` | `sub`、`target_peer_id`、`info_hash`、`connection_id`（即一次性 `jti`） | 数十秒 |
| TURN 中继授权 | 指定 `turn_region_id` | `relay` | `connection_id`、对端 `peer_id`、带宽/连接数/时长配额（以 `relay` JWT 或 TURN REST username/HMAC 承载） | 分钟级 |

每个服务**应当**至少校验：签名算法和签名、`iss`、`kid`、`aud`、`scope`、`sub`、`nbf`/`exp`；对于 `connect` JWT，还应校验 `target_peer_id`、`info_hash`，以及 `connection_id`（即 `jti`）的一次性消费语义。

## 7. 接口契约与运行边界

### 7.1 通用运行要求

Tracker API 与 `punch.enter` / `punch.leave` / `punch.signal` 的请求响应控制面经加密认证信道传输（TCP+TLS、UDP+DTLS 或 QUIC 均可，客户端与 Punch Server 间的常驻与瞬时信令信道同此要求）；为维持 NAT 映射的 `punch.heartbeat` 可以使用 UDP，但**必须**遵循前述 MAC、时钟窗口和序列号校验。消息使用版本化 schema；未知必填字段、超出大小限制的 candidate 列表和不匹配的 `connection_id` 都**必须**拒绝。每个响应至少带协议版本、请求 ID 和明确的错误码。所有时钟校验（`nbf`/`exp`、heartbeat 的时间戳窗口）使用统一的服务端可配置容差（如 ±60 秒），客户端**应当**与可信时间源对时，容差需计入 nonce 去重窗口与序列号乱序窗口的计算。

### 7.2 接口契约表

下表定义实现必须达成的最小契约；字段的具体编码可由后续 API 文档确定。

| 接口 | 请求中的关键字段 | 成功响应 / 幂等规则 | 授权与限制 |
| --- | --- | --- | --- |
| `tracker.register` | `peer_id`、`public_key`、签名、时间戳 | 入库；同 `peer_id` 重复注册幂等返回 | 验证 `peer_id == SHA-256(public_key)` 与签名；未注册或已过期才允许；TTL 90 天 |
| `tracker.revoke` | `peer_id`、`revocation_token` | 加入 `revoked` 列表；TTL 7 天 | 验证 token 签名；被吊销 `peer_id` 的所有后续请求拒绝 |
| `tracker.login` | 公钥、挑战签名 | session、enter JWT、服务配置 | 验证签名；对挑战一次性消费；`peer_id` 未被吊销；同时刷新 register TTL |
| `tracker.announce` | `add`/`del`、`info_hash` 集合、`complete`、TTL | 服务端裁剪后的过期时间；同一 peer/file 覆盖更新 | session `announce`；限制 TTL、批量大小、资源数和频率 |
| `tracker.query` | `info_hash`、候选数 | 有上限的候选页及每个目标的 connect JWT | session `query`；限流、防枚举、不得返回 candidates |
| `tracker.relay_credentials` | `connection_id` | 请求方自身的短期 TURN REST 凭据（或 `relay` JWT） | 仅连接双方；按签发记录校验 `sub` 与记录 TTL，并检查配额 |
| `punch.enter` | enter JWT、持钥证明（nonce、`punch_id`、时间戳签名） | `binding_id`、`binding_key`、过期时间 | JWT + 持钥证明；nonce 一次性消费；`peer_id` 未被吊销；新 binding 替换旧 binding |
| `punch.leave` | `binding_id`、seq、MAC | 确认下线；binding 立即失效 | 有效 binding；校验 MAC 与序列号 |
| `punch.heartbeat` | `binding_id`、seq、timestamp、MAC | 刷新后的过期时间 | 有效 binding；校验 MAC、时钟窗口和序列窗口 |
| `punch.signal` | type、`connection_id`、SDP / ICE candidate / 控制字段 | type 对应的转发确认/错误（首个 offer 的响应含 `signal_key`） | offer 验证 connect JWT；answer/candidate/cancel 仅接受已登记连接的双方；SDP 对 Punch Server 不透明；A 断线重连凭 `connection_id` + `signal_key` 恢复 |

### 7.3 客户端函数面

#### 7.3.1 Tracker 通信接口

客户端对 Tracker 的通信面只有**请求**与**响应**两类，没有 Relay——Tracker 不向客户端推送消息，两者之间也不维护常驻信道；`register` 与 `revoke` 是一次性的身份管理请求，不携带 session JWT；`login` 是会话级认证请求；其余请求均携带 session JWT，在有效期内按需调用。

| 函数 | 方向 | 说明 |
| --- | --- | --- |
| `SendRegisterReq` / `OnRegisterRsp` | client ↔ Tracker | `tracker.register`；首次启动或密钥轮换时调用，一次性 |
| `SendRevokeReq` / `OnRevokeRsp` | client → Tracker | `tracker.revoke`；私钥泄露时提交 revocation token，一次性 |
| `SendLoginReq` / `OnLoginRsp` | client ↔ Tracker | `tracker.login`；rsp 含 session JWT、enter JWT、Punch Server 分配与 STUN/TURN 配置 |
| `SendAnnounceReq` / `OnAnnounceRsp` | client ↔ Tracker | `tracker.announce`；`add` / `del` 批量登记与撤下资源 |
| `SendQueryReq` / `OnQueryRsp` | client ↔ Tracker | `tracker.query`；rsp 含候选 Peer 的 `peer_id`、公钥、Punch 地址与 connect JWT |
| `SendRelayCredentialsReq` / `OnRelayCredentialsRsp` | client ↔ Tracker | `tracker.relay_credentials`；rsp 仅含请求方自身的 relay 凭据 |

`login` 的挑战交互（获取挑战、签名应答）实现上可以是同一次连接内的两段式请求，不引入独立接口名。

#### 7.3.2 Punch 通信接口

客户端对 Punch Server 的通信面分为三类：**请求**（客户端发起，必有配对响应）、**响应**（对某个在途请求的确认或错误）、**Relay**（Punch Server 转发来的对端信令，客户端没有对应在途请求）。A / B 是运行时角色，不是两套接口——同一个 client 既维护到自己 Punch Server 的常驻信道（对应 B 角色），也在发起连接时临时连接目标 Punch Server（对应 A 角色）。表中以 A（发起方）、B（提供方）、PA / PB（各自与目标的 Punch Server）标注方向。

| 函数 | 方向 | 说明 |
| --- | --- | --- |
| `SendEnterReq` / `OnEnterRsp` | A ↔ PA；B ↔ PB | `punch.enter`；rsp 含 `binding_id`、`binding_key` |
| `SendLeaveReq` / `OnLeaveRsp` | A ↔ PA；B ↔ PB | `punch.leave` |
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

- 没有面向 Punch 的 `enter` JWT：攻击者可伪造任意 `peer_id` 绑定到 Punch Server，造成身份冒充、在线表污染或会话劫持。
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
