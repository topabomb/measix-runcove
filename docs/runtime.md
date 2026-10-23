# Server 运行时维护

本页维护 Runcove Server 的 MSB 来源、临时补丁、SQLite 防御及升级门槛。Standalone 不包含 MSB 或其数据库。部署操作见[部署指南](deployment.md)，故障处置见[运维手册](operations.md)，历史镜像和复现结果见[验证记录](validation-records.md)。

microsandbox 是本项目使用的上游 microVM 运行时；其 CLI/SDK 负责 VM、卷和 guest 连接，本项目负责用户空间、访问身份、任务及文件服务。感谢上游维护者与贡献者提供这一基础。项目介绍、相关生态与产品侧重点见[仓库首页](../README.md#microsandbox-与项目定位)，通用用法见[microsandbox 官方文档](https://docs.microsandbox.dev/getting-started/introduction)；下文只维护本项目锁定版本的适配和交付要求。

## 固定输入与构建身份

- SDK/CLI 锁定 `0.7.9`，Cargo 精确依赖与 `Cargo.lock` 一起维护，构建使用 `--locked`。版本锁定不表示它是上游最新版。
- [`msb-runtime.sha256`](../agent-space/msb-runtime.sha256) 校验官方种子 CLI/libkrunfw。种子 CLI 用于导出 guest 镜像，不能代表最终 VM 程序。
- SDK 的 `vendor/microsandbox`、`vendor/microsandbox-runtime` 是官方 crate 的审计副本，通过 Cargo 覆盖应用生命周期补丁。Docker 在固定官方源码及锁文件上重编译 CLI；源码归档、crate、副本树、补丁及每个修改文件的输入/结果摘要由 [`manifest.json`](../agent-space/docker/patches/manifest.json) 核验。
- [`apply-msb-patch.py`](../agent-space/docker/apply-msb-patch.py) 拒绝版本或内容不匹配，不允许模糊应用。服务与 CLI 均静态链接 SQLite 3.51.3；guest 保持匹配的官方来源。
- 镜像 `/opt/agent-space/runtime.json` 保存来源、补丁及最终哈希，`msb.sha256` 校验实际 CLI。entrypoint 覆盖数据卷中的旧缓存。验收须核对运行 VM 的 `/proc/<pid>/exe`，不能只检查 `/usr/local/bin/msb` 或种子摘要。

## 本地补丁

### Linux 运行身份

[`msb-0.7.9-lifecycle.patch`](../agent-space/docker/patches/msb-0.7.9-lifecycle.patch) 配对修改 SDK 和 runtime。官方 0.7.9 启动终态 sandbox 时可能等待被无关进程复用的历史 PID，`connect_or_start` 又可在终态持续重试 `SandboxStillRunning`，使请求超时且没有新 VM run。

统一观察器用 pidfd 固定观察对象；继承 fd 99 与预期 lifecycle lock 的 dev/ino 一致才证明所有权。明确的其他程序、其他空间或非首领线程可证明旧 PID 已被复用；进程退出为非活跃，无法证明则报错。锁文件名、墙钟差和裸 PID 存活均不足以证明身份。

启动忽略已证明复用的历史 PID，但保留原 `run.pid`；SDK 查询和 runtime 维护沿原有锁、重读及终态收敛流程更新状态。`connect_or_start` 只对权威状态确实为 Starting/Running 的并发启动继续观察；终态错误返回调用方。Agent Space 仅调用 SDK 公共 API，不修改 MSB 表、历史 PID、退出原因或 schema。

补丁只覆盖 Linux start/reconcile 和对应重试，不声称修复所有信号发送路径；旧版缺 fd 99、inode 不符等不确定身份保守报错。Windows/macOS 保持上游行为。真实 PID 复用已在隔离旧镜像复现；原事故 PID 131 的历史归属及最初 VM 退出原因仍未知。

### 已撤销的 Relay 回补

0.7.9 已收录迟到 BulkFinish（#1816）与部分帧读取取消安全（#1838）修复，因此删除旧 `msb-0.7.2-relay.patch` 及 manifest 条目，直接使用上游实现。构建仍运行完整 relay 回归，覆盖退休/取消 correlation、实例隔离、部分帧与下一帧对齐。文件服务的真实并发、取消、Range 和大文件验收继续保留。

应用自身的 TCP 半关闭和 DAV Session 连接恢复仍需保留，见[WebDAV 生命周期](webdav.md#5-session准入与取消)。VM Running 或 MCP 成功不能证明 DAV 连接健康。

## SQLite 防御

官方 0.7.2 数据库身份检查会打开并关闭普通数据库描述符。POSIX `close()` 可释放同进程的 SQLite 文件锁，另一进程关闭连接时可能删除仍被服务映射的 WAL/SHM。机制和原样 SDK 公共 API 已分别复现；历史现场缺少完整描述符证据，不能将所有 `malformed` 都归为此原因。参考 [SQLite 文件锁说明](https://www.sqlite.org/howtocorrupt.html#_posix_advisory_locks_canceled_by_a_separate_thread_doing_close_)及[固定调查证据](evidence/sqlite-20260930/README.md)。

0.7.9 已通过 `O_PATH` 修复上述 MSB 描述符问题，但其锁定的 `libsqlite3-sys 0.30.1` 内置 SQLite **3.46.0**，仍受另一项独立的 [WAL-reset 并发损坏问题](https://www.sqlite.org/wal.html#wal_reset_bug)影响。SQLite 官方给出的范围是 3.7.0 至 3.51.2，多连接分属不同线程/进程并发写入和 checkpoint 时可能触发；[3.51.3](https://www.sqlite.org/releaselog/3_51_3.html) 包含修复。MSB 数据库使用 WAL，符合需防御的使用方式；这不表示历史事故已被证明由该缺陷造成。

因此继续对服务与 CLI 静态链接 SQLite 3.51.3，不再把它解释为替代 MSB 描述符修复。仅升级服务不能覆盖 VM 的 `msb`。构建启用匹配的 `SQLITE_ENABLE_UNLOCK_NOTIFY`，设置 `SQLITE3_NO_PKG_CONFIG=1`，避免系统库替换固定静态输入。[`verify-sqlite-link.py`](../agent-space/docker/verify-sqlite-link.py) 在 strip 前读取真实 ELF `sqlite3_version` 符号并核对双端版本。

撤销条件：上游依赖自身携带已修复 WAL-reset 的 SQLite，核验服务及实际 VM 的链接版本，并通过多进程确认写入、SHM 生命周期、数据库替换检测及真实 VM 重启回归。届时删除自备 amalgamation、编译环境覆盖和专用版本约束；数据库完整性与数据保留验收继续保留。升级不能修复已有磁盘损坏，不热换连接池、自动重建空间或重放结果未知的命令。

## 上游跟踪

下表于 **2026-10-23** 核对上游 Issue/PR、v0.7.9 标签源码及修复提交祖先关系。当前实现锁定 0.7.9；合并、发布收录、候选镜像验收和生产切换分别记录，不能互相替代。

| 问题 | 上游状态与本地处理 |
|---|---|
| [#1642 运行身份](https://github.com/superradcompany/microsandbox/issues/1642) | Open；[#1685](https://github.com/superradcompany/microsandbox/pull/1685) 仍为 Draft。v0.7.9 终态启动仍等待裸历史 PID，`connect_or_start` 仍吞掉 `SandboxStillRunning`。官方二进制在隔离测试库注入无关存活 PID 后约 5 秒失败；这是合成陈旧 PID 夹具，不是真实内核 PID 复用。保留本地生命周期补丁 |
| [#1826 部分帧读取](https://github.com/superradcompany/microsandbox/issues/1826) | [#1838](https://github.com/superradcompany/microsandbox/pull/1838)，commit `8e419cb`，已收录 v0.7.9；目标源码 relay 回归通过。已撤销本地回补，保留候选镜像 DAV/VM 验收门槛 |
| [#1801 迟到 finish](https://github.com/superradcompany/microsandbox/issues/1801) | [#1816](https://github.com/superradcompany/microsandbox/pull/1816)，commit `e5f72485`，已收录 v0.7.9；退休、取消中及 incarnation 隔离回归通过。已撤销本地回补 |
| [#1709 SQLite 锁](https://github.com/superradcompany/microsandbox/issues/1709) | [#1766](https://github.com/superradcompany/microsandbox/pull/1766)，commit `c5c3fa7`，已收录 v0.7.8/v0.7.9。v0.7.9 的锁保留、跨进程确认写入、SHM 生命周期和替换检测回归通过；SDK 已更新；上游仍内置 SQLite 3.46.0，因独立 WAL-reset 缺陷保留双端 SQLite 3.51.3 构建约束 |

本次固定输入及实际范围见[验证记录](validation-records.md#msb-079-升级交付2026-10-23)。[v0.7.9](https://github.com/superradcompany/microsandbox/releases/tag/v0.7.9) 对本项目最直接的增益是 relay/数据库修复、执行缓冲限额及 `SandboxHandle::terminal_exit()`。可选输出背压 [#1832](https://github.com/superradcompany/microsandbox/pull/1832) 已被撤回；最终发布的是 [#1839](https://github.com/superradcompany/microsandbox/pull/1839) 固定额度流控：每空间 128 个执行、每执行 64 KiB 待交付输入和 1 MiB 未消费输出额度（含消息开销），16 KiB 分块，不是用户可配置参数。旧 peer 保持旧执行行为，不能只替换 CLI 就宣称应用获得该保证。退出原因 API 可用于区分闲置退出与异常退出，但不修复运行身份；应用输出截断、超时、后代清理及容量管理仍由现有所有者负责。 HTTP 策略拒绝可返回 403 和 `network_policy_denied`/`domain` 结构化字段，便于区分访问策略与链路故障；其他端口也可能在读取 HTTP 前直接拒绝连接。这不放开主机访问，网络回归同时检查受拒请求未到达本机监听端口。

已发布的[帧读取回补验证](https://github.com/superradcompany/microsandbox/issues/1826#issuecomment-6105858097)和[真实重启复现](https://github.com/superradcompany/microsandbox/issues/1642#issuecomment-6138707107)保留为上游上下文。旧 PR 的计划不能替代实际代码；补丁或版本变化时重新核查本表。文档维护不代替用户发布上游评论。

## 升级与逐项撤销

1. 对照目标发布的实际源码、SDK/CLI/guest 兼容性及每项修复，更新锁文件、源码/种子/副本/补丁摘要。剩余补丁重新审查并生成匹配版本，不能绕过旧版本拒绝检查。
2. 生命周期修补须同时覆盖 terminal run 身份、SDK/runtime 收敛和终态错误返回。串行运行身份回归及 [`test_dav_restart_vm.py`](../agent-space/tests/test_dav_restart_vm.py)：旧发布镜像创建的原盘、真实 PID 复用、无关进程存活、历史 PID/双盘保留、并发首次 DAV 单次启动、暂停异常重启、失败/取消恢复、容量和不确定身份。
3. 两项 relay 修补分别运行上表语义回归，再做并发 DAV 上传/PROPFIND、GET 摘要/Range、4 GiB 读取、取消及缓存断连恢复。SDK/MCP 成功不能代替文件验收。
4. 数据库修复须核对 identity capture/verify/Drop 不释放 SQLite 锁，替换检测仍有效；验证多进程确认写入不丢失、SHM 不提前删除及真实 VM 停止/重启。同时确认上游内置 SQLite 已修复 WAL-reset，满足上文条件再撤销自备 SQLite 构建，不能因 MSB Issue 关闭直接移除。
5. 每项独立撤销。两个 relay hunk 都不需要时才删除 relay patch/manifest 条目；两个 MSB vendor 无必要修改时才撤销 Cargo 覆盖；全部补丁及覆盖退出后才能删除相应审计步骤，语义回归继续保留。
6. 对同一候选镜像完成[发布资格](development.md#镜像资格与离线包)、精确旧版本升级与独立卷完整冷备回滚，记录来源、镜像和撤销清单后按[运维手册](operations.md)部署。既有用户、凭据、固定镜像和磁盘保持，不用版本默认值重建资源。
