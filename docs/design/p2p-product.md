# P2P 产品化规划

| 状态 | 最后更新 |
| --- | --- |
| 规划稿（Draft） | 2026-08-22 |

## 1. 产品定位

一句话：**P2P 基础设施 + SDK**。核心卖点是服务端不碰内容、带宽成本摊给用户网络。

- 对内容分发方：零带宽成本的分发通道（数据面 P2P 直连，服务端只做控制面）
- 对终端：浏览器原生可用（WebRTC 同栈，浏览器与 App 共用）
- 对运营方：控制面中心化、数据面去中心化——管理面可控，数据面免流量

对标路线：WebTorrent（基础设施 + SDK + 托管服务）。

## 2. 交付物全景

产品化交付物分三层：**必须自研的三件套**、**可复用的现成组件**、**文档之外的工程化补充**。

### 2.1 必须自研：核心三件套

| 组件 | 职责 | 为什么不能复用 |
| --- | --- | --- |
| **Tracker Server** | 控制面核心：身份注册表、资源索引（announce / query）、connection 记录、JWT 签发、nonce / jti 一次性语义、revocation（§7.2.1） | 协议完全自定义（Ed25519 身份 + JWT 授权 + info_hash 索引），BT tracker 不适用 |
| **Punch Server** | 信令中继：Binding 维护（join / heartbeat / exit）、常驻与瞬时信道、signal_key 断线恢复、connect JWT 的 jti 消费、容量控制（§7.2.2） | 自定义信令中继，比 STUN/TURN 复杂，无现成组件 |
| **Client SDK** | 身份管理 + 控制面通信 + WebRTC 数据面拼装（§7.3 三张函数面表格的完整实现） | 最大的工程量，见第 3 节 |

### 2.2 可复用组件

- **STUN / TURN**：coturn（协议文档 §4.7 已指定 static-auth-secret 方案）
- **WebRTC 栈**：浏览器原生；App 端用 libwebrtc / WebRTC.framework
- **JWT、Ed25519、bencode**：现成库

### 2.3 工程化补充（协议文档之外的必需物）

1. **运营面**：Tracker / Punch 管理控制台（Punch 清单管理、密钥轮换工具）、监控、审计——分布式基础设施没有运维面无法存活
2. **协议一致性测试套件**：三件套可独立实现，没有 conformance test，协议演进必坏（§7.1 声明消息版本化 schema，但未定义测试）
3. **参考实现客户端**：SDK 无法自证正确，需要至少一个完整参考客户端做互操作验证
4. **横向扩展入口**：负载均衡 / DNS 部署模板（协议契约只暴露逻辑服务地址，落地仍需部署层）

## 3. Client SDK 平台与语言面

SDK 是**最重的交付物**，文档 §7.3 的函数面表格只是接口骨架，底下还有：

- 身份管理：Ed25519 密钥对、peer_id 派生、revocation token 生成
- Punch 信道层：常驻 Binding 信道 + 瞬时信令信道 + `signal_key` 断线恢复
- WebRTC 数据面：双 DataChannel、block / piece 状态机（missing → partial → downloaded → verified）、bitfield / have、request / reject / cancel、顺序窗口 + rarest first + endgame 调度、magnet 模式、TURN 回退
- 文件 I/O：in-place 写盘、块哈希校验、bitmap 持久化

| 平台 | 语言 | 说明 |
| --- | --- | --- |
| Web | TypeScript | 浏览器原生 WebRTC，开发最顺，优先做 |
| iOS | Swift + WebRTC.framework | 原生 WebRTC 封装 |
| Android | Kotlin + libwebrtc | 原生 WebRTC 封装 |
| 桌面 | Electron（复用 JS）或原生 | 视目标用户决定 |
| 服务端 / 种子源托管 | Go / Rust | CDN 兜底虚拟 Peer（§5.1 `cdn_list`）的托管实现 |

## 4. 实施路径（MVP）

```
Phase 1：Tracker + Punch + JS SDK + 浏览器 Demo + coturn（复用）
         —— 浏览器原生 WebRTC 免掉 ICE/DTLS 平台差异，专注验证协议闭环
            （控制面 + 消息语义 + 状态机），一个语言跑通全链路
Phase 2：一致性测试套件 + 参考客户端 + 可观测性
         —— 协议稳定性的前提；可观测性覆盖 Tracker/Punch 运营面
Phase 3：iOS / Android SDK + 桌面端
         —— 原生 WebRTC 封装，多平台行为差异处理
Phase 4：托管服务（SaaS 化：代运营 Tracker/Punch）+ 私有化部署
         —— 商业变现层
```

平台优先序的依据：JS SDK 能用浏览器原生 WebRTC 免掉最复杂的 ICE / DTLS 实现，让 Phase 1 专注验证协议本身，而不是被平台差异拖住。

## 5. 风险与关键点

| 风险 | 说明 | 缓解 |
| --- | --- | --- |
| WebRTC 跨平台行为差异 | 不同平台 ICE / DTLS / SCTP 实现行为不一致 | 一致性测试套件 + 参考客户端互操作验证 |
| 状态机与调度正确性 | block / piece 状态机 + endgame / rarest first / 顺序窗口组合是纯逻辑但极易出错 | Phase 1 优先实现并压力测试 |
| 断线重连 | `signal_key` 恢复、瞬时信道补发逻辑（§3.5） | 早期引入网络故障注入测试 |
| 协议演进 | 版本化 schema 定义了机制但未定义治理 | Phase 2 定协议版本策略 |

## 6. 商业形态

三档并行：

1. **托管服务**：代运营 Tracker / Punch（SaaS 化），按身份数 / 并发连接 / 转发量计费
2. **SDK 授权**：按平台 / 应用授权
3. **私有化部署**：面向内容分发方的整包交付（Tracker + Punch + 部署模板）

成本结构优势：数据面带宽成本由用户网络承担，服务端只有控制面流量——单位用户成本随规模递减，这是产品经济的核心卖点。
