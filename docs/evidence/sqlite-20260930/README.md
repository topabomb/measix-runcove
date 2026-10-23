# SQLite 机制验证证据（2026-09-30）

本目录只保存隔离测试的源码与结果；真实账号、生产磁盘清单与本机恢复快照不公开。生产数据库、完整日志、配置和用户凭证不在 Git 中；原始现场副本保存在受保护的外部备份目录。机制与构建限制见[运行时维护](../../runtime.md#sqlite-防御)，处置步骤见[运维手册](../../operations.md#sqlite-故障处置)。

| 文件 | 用途与结果 |
| --- | --- |
| [mechanism-results.json](mechanism-results.json) | 10 轮最小机制对照：混用旧写入进程且关闭普通数据库描述符时均失败；metadata 对照和统一 SQLite 3.51.3 均通过。 |
| [mechanism-repro.c](mechanism-repro.c) | Linux/POSIX 文件锁机制复现源码；测试创建独立数据库，不用于操作业务数据库。 |
| [sdk-346-results.json](sdk-346-results.json) | 原样官方 SDK 0.7.2 公共 API，10/10 删除仍持有的 SHM、丢失父进程确认写入。结构完整性检查仍为 ok。 |
| [sdk-351-results.json](sdk-351-results.json) | 同一 SDK 源码，SQLite 3.51.3，10/10 保留全部确认写入。 |
| [sdk-public-api-repro.rs](sdk-public-api-repro.rs) | SDK 多进程复现源码，不包含 Agent Space 或直接数据库写入。 |
| [release-vm-results.json](release-vm-results.json) | release 运行时：10 轮双 VM 启停及容器重启，共 22 次运行，工作区数据保留、完整性通过、malformed=0。 |
| [legacy-repair-results.json](legacy-repair-results.json) | 原静态用户版本的运行时修复及完整冷备份独立卷恢复演练；原 Token、四工具、根盘和工作区文件通过。 |
| [normal-image-vm-results.json](normal-image-vm-results.json) | 常规完整镜像：3 轮双 VM 及重启，共 8 次运行；覆盖禁用、空闲回收和持久化。 |
| [checker-results.json](checker-results.json) | 只读工具拒绝缺库、损坏和外键违规，原文件不变，缺库不被创建。 |
| [review-validation-results.json](review-validation-results.json) | 提交前审查回归：完整构建、未知运行状态、嵌套挂载、日志 stderr、旧版修复/回滚及校验失败停机。 |
| [build-guard-results.json](build-guard-results.json) | 拒绝覆盖已有镜像、拒绝未经验证的源码，保留原镜像不可变回滚标签。 |

JSON 保留当时镜像、二进制哈希、测试次数与结果。临时目录路径仅表示隔离试验位置，目录本身已清理；其中的镜像名称不替代实际部署身份。新的真实 VM 验证可在 `agent-space/` 执行：

```bash
python3 docker/verify-sqlite-runtime.py --image YOUR_VALIDATED_IMAGE --label release --rounds 10
```

旧版 SQLite-only 修复脚本已随 0.7.9 升级撤销，历史结果不变；现行升级/回滚走完整镜像流程。测试使用自己创建的数据卷与账号。不要把生产数据卷挂到验证容器，也不要将冷备份或 Docker 环境变量附到公开 issue。当前规避通过这些有限测试，不是所有损坏路径均已消除的证明。
