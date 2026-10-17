# 有边界的并发上传

第一阶段：单 Client 单文件发布，同时服务多个下载方；默认上传连接额度 4，范围 1–16，独立于下载并发。额度覆盖握手、传输和 VOD 空闲连接；低层 ControlSession/PeerSession 调用者自行管理任务与并发。

- [x] Punch 常驻信道统一分发 Connect，其他消息按 connection_id 路由；有界队列、重复请求隔离、异常和关闭传播。
- [x] 接受连接前预留 Client 上传额度；容量满返回已有 Busy 信令，下载端提供明确 Busy 错误。第一版不自动排队/无限重试。
- [x] 并发握手与独立上传 worker，远端网络/协议错误隔离；源文件读取失败或变化停止整个发布并撤销宣告。
- [x] 共享源文件短临界区读取，不在网络 I/O 时持锁；发布时构建一次 ProofTree。
- [x] close/Drop 停止接入、取消并等待全部 worker，等待在途阻塞读取后释放发布及额度；继续复用共享心跳。
- [x] 回归与真实联机：多个并发下载方、额度拒绝与释放、同时握手信令不串线、慢/坏节点隔离、取消清理、源文件变化、续租。

热点缓存方案与验证见 [upload-block-cache.md](./upload-block-cache.md)。并发按偏移读取见 [positional-source-reads.md](./positional-source-reads.md)。

第二阶段总上传限速已接入，设计与验证见 [upload-rate-limit.md](./upload-rate-limit.md)。

## 验证记录

- `cargo test --workspace --all-features --offline`：281 项通过；两个 Redis 集成测试及一个本地服务测试默认忽略。
- 本地 Track/Punch 联机测试单独执行，两次通过：并发握手和完整下载、Busy 与额度复用、VOD 空闲占位、关闭/重新发布、源文件变化后撤销。
- 新增路由隔离、队列上限、慢拒绝、暂停接入、阻塞读取清理及 Builder 参数测试。SDK 普通测试共 61 项通过。
- 全工作区 Clippy（所有 targets/features、`-D warnings`）、fmt、diff 检查通过。
- 中英文文档构建及 transport 页面浏览器检查通过。
- 联机回归发现并修复 Punch Reject 删除连接后无法转发的问题，补充服务端回归测试。

容量满后第一版不会自动等待或重试；单源下载可能返回 Busy，应用可显式重试。SDK 低层接入 API 自行管理并发，不受托管发布额度限制。


多内容路由与下载中共享扩展见 [partial-peer-sharing.md](./partial-peer-sharing.md)。第一阶段的单文件约束在该扩展中解除，上传额度继续按 Client 共享。
