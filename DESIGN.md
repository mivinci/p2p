# P2P 连接流程与授权设计

本文统一使用以下名称：

- **Tracker Server**：身份认证、资源索引、Peer 发现与 JWT 签发。
- **Punch Server**：维护 Peer 的在线 Binding，转发打洞信令。
- **STUN Server**：协助客户端收集公网候选地址。
- **TURN / Relay Server**：无法直连时转发数据流。

`peer_id` 应由客户端长期保存的公钥派生，而非由 Tracker 临时分配。客户端分别登录并绑定到各自的 Punch Server；一次下载连接中，A 会直接联系 B 所在的 Punch Server。

## 完整流程

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
        A->>T: login(peer public key, signature, capabilities)
        T-->>A: session JWT；bind JWT（aud=Punch A，scope=bind/keepalive）；STUN/TURN 配置
        A->>PA: bind(bind JWT)
        PA-->>A: binding_id、binding_key、expires_at
        A->>STUN: Binding Request
        STUN-->>A: A candidates（host / srflx）
    and B 上线并登记资源
        B->>T: login(peer public key, signature, capabilities)
        T-->>B: session JWT；bind JWT（aud=Punch B，scope=bind/keepalive）；STUN/TURN 配置
        B->>PB: bind(bind JWT)
        PB-->>B: binding_id、binding_key、expires_at
        B->>STUN: Binding Request
        STUN-->>B: B candidates（host / srflx）
        B->>T: report(file_id, availability, TTL)
    end

    Note over A,PB: A / B 定期调用 keepalive(binding_id, seq, MAC)，刷新 Binding 与 NAT 映射

    A->>T: query(file_id)
    T-->>A: B peer_id、Punch B 地址、connect JWT（aud=Punch B，scope=connect，target=B）
    Note over T,A: 不返回 B 的公网地址或候选地址

    A->>PB: signal(type=offer, A candidates, connect JWT)
    PB->>PB: 验签；校验 aud、scope、sub、target、exp、jti
    PB->>B: signal(type=offer, A peer_id, A candidates)
    B->>PB: signal(type=answer, connection_id, B candidates)
    PB->>PB: 检查 B 仍处于有效 Binding
    PB-->>A: signal(type=answer, connection_id, B peer_id, B candidates)

    par UDP 打洞
        A->>B: UDP probe / authenticated handshake
    and
        B->>A: UDP probe / authenticated handshake
    end

    alt 直连成功
        A<<->>B: 端到端加密的数据通道（如 QUIC）
        B-->>A: 传输文件分块；A 校验内容哈希
    else 直连失败
        A->>T: relay_credentials()
        T-->>A: relay JWT（aud=TURN，scope=relay）或 TURN REST 凭据
        B->>T: relay_credentials()
        T-->>B: relay JWT 或 TURN REST 凭据
        A<<->>TURN: 加密数据流
        TURN<<->>B: 加密数据流
    end
```

## 各步骤说明与设计原因

### 1. 登录与身份建立

客户端携带公钥及对挑战或请求的签名登录 Tracker。Tracker 验证身份后，返回本次会话可使用的 JWT、分配的 Punch Server 地址，以及 STUN/TURN 配置。

这样设计的目的，是让 `peer_id` 保持长期稳定，同时让短期访问权可过期、可轮换。Tracker 不是文件数据的中转站，而是身份、发现与授权控制面。

### 2. `bind` 与 `keepalive`

客户端使用仅面向自身 Punch Server 的 `bind` JWT 建立 Binding。Punch Server 验签后创建 `binding_id`，并可返回会话级 `binding_key`。此 Binding 代表“该 peer 当前可通过这个 Punch Server 被联系到”。

之后的保活使用 `keepalive(binding_id, seq, MAC)`，而不是每次发送完整 JWT。这样既刷新 NAT 映射和在线 TTL，也避免频繁验签与较大的 UDP 包。`seq` 和 MAC 用于防篡改与防重放。

### 3. STUN 收集候选地址

客户端自行向 STUN Server 收集候选地址，例如局域网地址（host candidate）与公网映射地址（server-reflexive candidate）。

Tracker 不保存或下发这些地址：候选地址会随网络切换而变化，且提前暴露会扩大隐私泄露与扫描风险。候选地址只在获得连接授权后的 Punch 信令中交换。

### 4. 资源登记与查询

B 使用 `report(file_id, availability, TTL)` 登记自己可提供的资源；记录必须带 TTL，离线后自动失效。A 使用 `query(file_id)` 查询少量候选 Peer。

Tracker 的查询结果仅包含 B 的 `peer_id`、B 所在 Punch Server 地址和一次连接所需的 `connect` JWT；**不包含 B 的公网地址或候选地址**。限制候选数量也能避免热门资源的索引放大和完整 Peer 列表泄露。

### 5. 通过 Punch B 交换候选地址

A 通过 `punch.signal` 发送 `type=offer`，其中包含自己的候选地址与 `connect` JWT。Punch B 验证该 JWT 后，才向 B 转发该 offer。B 再通过同一个 `punch.signal` 接口发送 `type=answer`，携带最新的 `B candidates` 与 `connection_id`；Punch B 将其转发给 A。

`offer`、`answer`、`candidate` 和 `cancel` 都是 `punch.signal` 的消息类型，而不是独立接口。若支持 ICE trickle，后续新增候选地址也使用 `type=candidate` 和同一个 `connection_id`。

因此，A 直接请求 **Punch B**，而不是先经由 Punch A 再转发。Punch A 的职责仅是维护 A 自己的在线 Binding；Punch B 则因为持有 B 的有效 Binding，能够把信令送达 B。

### 6. 同时 UDP 打洞与直连

拿到彼此候选地址后，A 与 B 同时向对方发送 UDP 探测包并进行端到端身份认证握手。两端同时发包能帮助各自 NAT 建立允许回包的映射。

直连成功后，文件分块走端到端加密通道，Punch Server 不参与文件传输。文件内容应通过分块哈希或 Merkle 树校验，不信任任一单独 Peer 提供的数据。

### 7. TURN 回退

对称 NAT、企业防火墙或 CGNAT 等环境可能导致打洞失败。这时双方获取面向 TURN 的独立中继授权，然后经 TURN 传输加密数据。

TURN 只转发密文流量，不能成为内容可信边界。若使用标准 coturn，通常由 Tracker 根据同一套授权决策换发短期 TURN REST 凭据；若 TURN 支持 JWT 鉴权，则可以直接使用 `relay` JWT。

## JWT 的统一模型

所有授权票据可使用同一种 JWT 格式与同一套 Tracker 签名密钥体系，但必须签发为不同、最小权限的 JWT 实例。

| JWT 用途 | `aud` | `scope` | 关键附加 claims | 典型有效期 |
|---|---|---|---|---|
| Tracker API 会话 | `tracker` | `query`、`report` | `sub` | 分钟级至小时级 |
| 建立/刷新 Punch Binding | 指定 `punch_id` | `bind`、`keepalive` | `sub` | 10–30 分钟 |
| 请求连接目标 Peer | 指定 `punch_b_id` | `connect` | `sub`、`target_peer_id`、`jti` | 数十秒 |
| 使用 TURN 中继 | 指定 `turn_region_id` | `relay` | 带宽/连接数配额 | 分钟级 |

每个服务应至少校验：签名算法和签名、`iss`、`kid`、`aud`、`scope`、`sub`、`nbf`/`exp`；对于 `connect` JWT，还应校验 `target_peer_id` 与一次性 `jti`。

### 没有这些 JWT / 授权机制会怎样

- 没有面向 Punch 的 `bind` JWT：攻击者可伪造任意 `peer_id` 绑定到 Punch Server，造成身份冒充、在线表污染或会话劫持。
- 没有 `keepalive` 的 MAC、序列号和过期管理：攻击者可伪造或重放保活包，使虚假 Binding 长期存在，或干扰真实 Peer 的可达性。
- 没有针对 B 的短期 `connect` JWT：任何人只要知道 Punch B 地址，就能要求它向 B 投递信令；Punch Server 会被用于骚扰、扫描、反射流量或对大量 Peer 发起连接请求。
- 只靠 `scope=connect`、却不校验 `aud` 与 `target_peer_id`：一张泄露票据可被拿去联系其他 Punch 节点或其他 Peer，权限范围过大。
- 没有 `exp`、`jti`：截获的连接请求可在很久以后无限重放。
- 没有独立的 TURN 授权与配额：攻击者可把 TURN 当开放代理消耗带宽，造成高额成本与滥用风险。
- 没有端到端身份认证和加密：即便 Punch/TURN 已正确鉴权，仍无法阻止恶意中继或网络路径窥探、篡改、冒充对端。

## 建议的接口命名

```text
tracker.login
tracker.report
tracker.query
tracker.relay_credentials

punch.bind
punch.keepalive
punch.signal
```
