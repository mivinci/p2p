# 有边界的按偏移源读取

## 目标与实现

- Unix/Windows 每次以绝对偏移读取，取消共享 seek/read 游标锁；其他平台保留串行兼容实现。
- PreparedFile 以 Arc 共享同一已打开文件、Seed 和不可变 ProofTree，克隆不重新打开路径、不复制整棵树。
- read_verified_block 改为只读 &self，独立校验每个块；读取前后检查文件长度/mtime，提前 EOF 与叶子不匹配返回 SourceChanged。
- Builder::max_upload_reads 默认 4，范围 1–16，独立于连接数及速率。同一 Client 准备的所有源句柄共用预算。
- SDK 在 spawn_blocking 前领取读取额度；排队者可取消。阻塞任务自己持有读取额度、上传连接额度和 TaskScope 守卫，按此顺序释放。网络 I/O 不占读取额度。
- 托管发布与上传池直接共享 Arc<PreparedFile>。低层 serve_file 原参数兼容，仅在开始时克隆一次 Arc 支撑的源句柄；同步读取 API 调用者自行限制线程并发。
- 不新增热点缓存，不承诺磁盘吞吐按连接数增长。源文件路径替换后仍服务原打开文件。

## 平台说明

- Unix read_at 不改变共享游标：[Rust FileExt](https://doc.rust-lang.org/std/os/unix/fs/trait.FileExt.html#tymethod.read_at)。
- Windows seek_read 的偏移独立于当前游标，但会更新游标，上传代码不依赖其值：[Rust FileExt](https://doc.rust-lang.org/std/os/windows/fs/trait.FileExt.html#tymethod.seek_read)。Windows 分支本轮未运行实机验证。

## 验证

- [x] 多线程交错读取不同块及短尾块；克隆共享文件、树、Seed、额度；Unix 游标保持。
- [x] 短读、Interrupted、提前 EOF；长度变化、篡改后恢复时间戳、空文件及越界；Unix 路径替换。
- [x] 共享读取预算、排队取消、Client 多个源句柄共用额度；阻塞读取退出前保留两个预算，关闭后释放。
- [x] Builder 默认值、独立配置与 1–16 边界。
- [x] 本地 Track/Punch 并发下载与两个 VOD 连接向不同块随机跳转，包括短尾；源变化、关闭、重新发布、统计及接入所有权回归。
- [x] 全工作区测试、Clippy、fmt、文档及页面检查。

## 验证结果

- 工作区 all-features：315 项通过（SDK 94 项）；Redis 集成测试未重跑。
- 本地 Track/Punch/WebRTC 两次通过，双 VOD 连接并行向不同块跳转，覆盖末尾 13 字节块；并回归限速、公平、Busy、关闭、重发布、源变化、统计和接入租约。
- 读取/上传额度与阻塞任务清理测试连续 20 次通过。
- Clippy（所有 targets/features、拒绝警告）、fmt、diff 检查、中英文文档构建和 transport 两页浏览器检查通过。
- macOS 本地运行；Linux 由 PR CI 回归。Windows 仅提供标准库适配，本轮未运行 Windows 实机测试。没有引入块缓存或变更网络消息格式。


后续托管上传在读取边界加入有界热点缓存和同块合并，见 [upload-block-cache.md](./upload-block-cache.md)；同步 PreparedFile 与低层上传的读取语义不变。
