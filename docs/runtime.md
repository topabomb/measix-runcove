# Server 运行时维护

本页维护 Runcove Server 的 MSB 来源、临时补丁、SQLite 防御及升级门槛。Standalone 不包含 MSB 或其数据库。部署操作见[部署指南](deployment.md)，故障处置见[运维手册](operations.md)，历史镜像和复现结果见[验证记录](validation-records.md)。

microsandbox 是本项目使用的上游 microVM 运行时；其 CLI/SDK 负责 VM、卷和 guest 连接，本项目负责用户空间、访问身份、任务及文件服务。感谢上游维护者与贡献者提供这一基础。项目介绍、相关生态与产品侧重点见[仓库首页](../README.md#microsandbox-与项目定位)，通用用法见[microsandbox 官方文档](https://docs.microsandbox.dev/getting-started/introduction)；下文只维护本项目锁定版本的适配和交付要求。

## 固定输入与构建身份

- SDK/CLI 锁定 `0.7.2`，Cargo 精确依赖与 `Cargo.lock` 一起维护，构建使用 `--locked`。版本锁定不表示它是上游最新版。
- [`msb-runtime.sha256`](../agent-space/msb-runtime.sha256) 校验官方种子 CLI/libkrunfw。种子 CLI 用于导出 guest 镜像，不能代表最终 VM 程序。
- SDK 的 `vendor/microsandbox`、`vendor/microsandbox-runtime` 是官方 crate 的审计副本，通过 Cargo 覆盖应用生命周期补丁。Docker 在固定官方源码及锁文件上重编译 CLI；源码归档、crate、副本树、补丁及每个修改文件的输入/结果摘要由 [`manifest.json`](../agent-space/docker/patches/manifest.json) 核验。
- [`apply-msb-patch.py`](../agent-space/docker/apply-msb-patch.py) 拒绝版本或内容不匹配，不允许模糊应用。服务与 CLI 均静态链接 SQLite 3.51.3；guest 保持匹配的官方来源。
- 镜像 `/opt/agent-space/runtime.json` 保存来源、补丁及最终哈希，`msb.sha256` 校验实际 CLI。entrypoint 覆盖数据卷中的旧缓存。验收须核对运行 VM 的 `/proc/<pid>/exe`，不能只检查 `/usr/local/bin/msb` 或种子摘要。

## 本地补丁

### Linux 运行身份

[`msb-0.7.2-lifecycle.patch`](../agent-space/docker/patches/msb-0.7.2-lifecycle.patch) 配对修改 SDK 和 runtime。官方 0.7.2 启动终态 sandbox 时可能等待被无关进程复用的历史 PID，`connect_or_start` 又可在终态持续重试 `SandboxStillRunning`，使请求超时且没有新 VM run。

统一观察器用 pidfd 固定观察对象；继承 fd 99 与预期 lifecycle lock 的 dev/ino 一致才证明所有权。明确的其他程序、其他空间或非首领线程可证明旧 PID 已被复用；进程退出为非活跃，无法证明则报错。锁文件名、墙钟差和裸 PID 存活均不足以证明身份。

启动忽略已证明复用的历史 PID，但保留原 `run.pid`；SDK 查询和 runtime 维护沿原有锁、重读及终态收敛流程更新状态。`connect_or_start` 只对权威状态确实为 Starting/Running 的并发启动继续观察；终态错误返回调用方。Agent Space 仅调用 SDK 公共 API，不修改 MSB 表、历史 PID、退出原因或 schema。

补丁只覆盖 Linux start/reconcile 和对应重试，不声称修复所有信号发送路径；旧版缺 fd 99、inode 不符等不确定身份保守报错。Windows/macOS 保持上游行为。真实 PID 复用已在隔离旧镜像复现；原事故 PID 131 的历史归属及最初 VM 退出原因仍未知。

### Relay

[`msb-0.7.2-relay.patch`](../agent-space/docker/patches/msb-0.7.2-relay.patch) 含两个可独立撤销的 hunk：

| 修补 | 保留的语义与回归 |
|---|---|
| 迟到 BulkFinish | 同一 client incarnation 已退休或取消中的 correlation 丢弃迟到 finish；取消流保留路由等待普通 terminal。未知 correlation、活动流 acceptance/offset/重复 finish 校验仍严格；覆盖容量释放和实例隔离 |
| 部分帧读取取消安全 | `client_reader_task` 保留同一个帧读取 future，处理 local-shm release 或非关闭 watch 后继续读取。覆盖部分头/正文、多次 release、下一帧对齐、断开、EOF 和非法长度 |

补丁不改 wire 格式、不重放操作，保留 bulk 数据通路，不增加 Base64 或额外数据复制。构建运行 relay 和新增回归。旧读取模式的确定性测试失败，保留 future 后通过；事故未保存完整逐帧跟踪，不能把所有停止逐一归因于相同顺序。

应用自身的 TCP 半关闭和 DAV Session 连接恢复仍需保留，见[WebDAV 生命周期](webdav.md#5-session准入与取消)。VM Running 或 MCP 成功不能证明 DAV 连接健康。

## SQLite 防御

官方 0.7.2 数据库身份检查会打开并关闭普通数据库描述符。POSIX `close()` 可释放同进程的 SQLite 文件锁，另一进程关闭连接时可能删除仍被服务映射的 WAL/SHM。机制和原样 SDK 公共 API 已分别复现；历史现场缺少完整描述符证据，不能将所有 `malformed` 都归为此原因。参考 [SQLite 文件锁说明](https://www.sqlite.org/howtocorrupt.html#_posix_advisory_locks_canceled_by_a_separate_thread_doing_close_)及[固定调查证据](evidence/sqlite-20260930/README.md)。

本项目使用双端 SQLite 3.51.3 防御，不自行改写数据库身份处理。仅升级服务不能覆盖 VM 的 `msb`；仅完整性检查通过也不能证明历史确认写入未丢失。构建启用匹配的 `SQLITE_ENABLE_UNLOCK_NOTIFY`，运行时构建设置 `SQLITE3_NO_PKG_CONFIG=1`，避免系统库替换固定静态输入。[`verify-sqlite-link.py`](../agent-space/docker/verify-sqlite-link.py) 在 strip 前读取真实 ELF `sqlite3_version` 符号并核对双端版本，不能通过 grep 内嵌字符串判断。

双端升级在机制、SDK 多进程和真实 VM 的固定对照中通过，但不修复已有真实磁盘损坏，也不替代上游描述符修复。保留元数据读取错误、未知状态容量和原盘；不热换连接池、自动重建空间或重放结果未知的命令。

## 上游跟踪

下表于 **2026-10-16** 核对上游 Issue/PR 和发布页。项目仍锁定 0.7.2；合并、发布收录和本项目验收是三个独立条件，表中合并状态不代表本项目已采用。

| 问题 | 上游状态与本地处理 |
|---|---|
| [#1642 运行身份](https://github.com/superradcompany/microsandbox/issues/1642) | Open；[#1685](https://github.com/superradcompany/microsandbox/pull/1685) 仍为 Draft，范围以 replace/kill/drain 为主。保留本地生命周期补丁；需覆盖 terminal start、SDK/runtime 收敛和终态重试返回 |
| [#1826 部分帧读取](https://github.com/superradcompany/microsandbox/issues/1826) | 已由 [#1838](https://github.com/superradcompany/microsandbox/pull/1838) 合并，commit `8e419cb`；保留 0.7.2 回补，升级时确认目标发布收录后再验收 |
| [#1801 迟到 finish](https://github.com/superradcompany/microsandbox/issues/1801) | 已由 [#1816](https://github.com/superradcompany/microsandbox/pull/1816) 合并；保留 0.7.2 回补，独立验证 correlation 退休/取消行为 |
| [#1709 SQLite 锁](https://github.com/superradcompany/microsandbox/issues/1709) | 已由 [#1766](https://github.com/superradcompany/microsandbox/pull/1766) 合并，Linux O_PATH 修复列入 [v0.7.8](https://github.com/superradcompany/microsandbox/releases/tag/v0.7.8)。尚未替换本项目锁定 SDK；SQLite 防御不等于已经采用此代码修复 |

已发布的[帧读取回补验证](https://github.com/superradcompany/microsandbox/issues/1826#issuecomment-6105858097)和[真实重启复现](https://github.com/superradcompany/microsandbox/issues/1642#issuecomment-6138707107)保留为上游上下文。旧 PR 的计划不能替代实际代码；补丁或版本变化时重新核查本表。文档维护不代替用户发布上游评论。

## 升级与逐项撤销

1. 对照目标发布的实际源码、SDK/CLI/guest 兼容性及每项修复，更新锁文件、源码/种子/副本/补丁摘要。剩余补丁重新审查并生成匹配版本，不能绕过旧版本拒绝检查。
2. 生命周期修补须同时覆盖 terminal run 身份、SDK/runtime 收敛和终态错误返回。串行运行身份回归及 [`test_dav_restart_vm.py`](../agent-space/tests/test_dav_restart_vm.py)：旧发布镜像创建的原盘、真实 PID 复用、无关进程存活、历史 PID/双盘保留、并发首次 DAV 单次启动、暂停异常重启、失败/取消恢复、容量和不确定身份。
3. 两项 relay 修补分别运行上表语义回归，再做并发 DAV 上传/PROPFIND、GET 摘要/Range、4 GiB 读取、取消及缓存断连恢复。SDK/MCP 成功不能代替文件验收。
4. 数据库修复须核对 identity capture/verify/Drop 不释放 SQLite 锁，替换检测仍有效；验证多进程确认写入不丢失、SHM 不提前删除及真实 VM 停止/重启。再决定是否撤销规避构建，不能因 Issue 关闭直接移除。
5. 每项独立撤销。两个 relay hunk 都不需要时才删除 relay patch/manifest 条目；两个 MSB vendor 无必要修改时才撤销 Cargo 覆盖；全部补丁及覆盖退出后才能删除相应审计步骤，语义回归继续保留。
6. 对同一候选镜像完成[发布资格](development.md#镜像资格与离线包)、精确旧版本升级与独立卷完整冷备回滚，记录来源、镜像和撤销清单后按[运维手册](operations.md)部署。既有用户、凭据、固定镜像和磁盘保持，不用版本默认值重建资源。
