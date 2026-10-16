# Measix Runcove

产品提供 Runcove Server/Workspace（托管多空间，MSB VM + 管理 API）和 Runcove Standalone（一个容器直接运行一个使用者的一个空间，无 MSB/KVM、无管理 API、无静态多空间配置）。两个入口共用核心，不复制实验 Node MCP/任务服务。产品与文档入口见 `README.md`，验证范围见 `docs/validation-records.md`。外部研究目录只作只读参考，正式构建不得依赖它或个人运行数据。

> Rust workspace: `agent-engine` + `agent-space-mcp` + `agent-space-static-http` + `agent-space-webdav` + `agent-space-desktop`

## Environment

- OS: WSL Ubuntu, bash shell
- Root: `agent-space/`（相对仓库根目录）

## Build & Test

```bash
cargo check --locked # 快速语法检查
cargo build --release --locked
cargo build -p agent-space-static-http -p agent-space-webdav -p agent-space-desktop --locked # 真实 VM 测试使用当前 helper 二进制
cargo test --lib --locked # 单元测试
cargo test --test '*' --locked # 全部测试
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build -p agent-space-mcp --no-default-features --bin runcove-standalone --locked # 不链接 MSB 的独立入口
python3 tests/test_managed_cleanup.py # guest 暂存清理归属与故障回归
python3 tests/test_exec_owner.py # 有界输出及前台后代清理
# browser/files/lifecycle/CLI和desktop-ui端到端测试须用独立资格Host，见浏览器合同
```

## Runtime Tool

- Python 3.12: `/usr/bin/python3`（venv: `.venv`）
- microsandbox SDK / CLI: `0.7.2`；CLI `msb`（通过 PATH 查找）
- `Cargo.toml` 精确锁定 SDK，`Cargo.lock` 入库；构建使用 `--locked`。SDK 默认 `prebuilt` 构建会同步 `$MSB_HOME`（默认 `~/.microsandbox`）内 CLI / libkrunfw。
- Linux x86_64 发布运行时校验：`agent-space/msb-runtime.sha256`。
- 上述校验是官方种子 CLI/libkrunfw 输入；SDK 以官方 0.7.2 crate 为基础，通过 Cargo vendor 覆盖应用 Linux 生命周期临时补丁；Agent Space 不直接写 MSB 数据库。Docker 内实际 VM msb 从锁定源码应用 `agent-space/docker/patches/` 的 relay 补丁（迟到 BulkFinish 与部分帧读取取消安全）及生命周期补丁后重编译，与服务均使用 SQLite 3.51.3；发布 crate、源码副本、补丁与文件结果摘要均经 manifest 校验；来源、补丁和最终哈希见镜像 `/opt/agent-space/runtime.json`。升级前先核对 `docs/runtime.md` 的逐项补丁撤销条件。
- 先构建当前 helper，再串行运行真实 VM 测试：`cargo build -p agent-space-static-http -p agent-space-webdav -p agent-space-desktop --locked`、`cargo test --workspace --locked -- --test-threads=1`，避免各测试清理相互干扰。
- WSL 构建资源紧张时可设置 `CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`，避免测试链接产物耗尽 Windows 宿主磁盘；不要同时跑多个完整构建。
- 镜像: `nikolaik/python-nodejs:python3.12-nodejs22-slim`
- 镜像默认 shell 为 dash，项目已通过 `config.toml` 指定 `/bin/bash`

## Key Docs

- `README.md` — 产品、部署选择与文档入口；文档双语规划见开发指南
- `docs/development.md` — 代码职责、回归矩阵、发布资格与文档维护
- `docs/architecture.md` — 共享核心、生命周期及受管进程；修改 helper/worker 或 Agent 说明前核对受管进程章节
- `docs/deployment.md` — 两目标构建、配置、首次启动与离线包
- `docs/operations.md` — 升级、迁移、完整冷备回滚、工作区恢复与 SQLite 故障处置
- `docs/server-api.md` — Server 管理 API、凭据、资源摘要与平台集成
- `docs/browser-desktop.md` — 浏览器工具、任务、控制权、工作台与文件流
- `docs/webdav.md` — DAV 文件语义、4 GiB 读取、客户端与能力边界
- `docs/web-publish.md` — 临时发布、认证、生命周期与同源边界
- `docs/runtime.md` — Server MSB 来源、补丁、上游跟踪和逐项撤销条件；升级或补丁变更前核对
- `docs/validation-records.md` — 固定源码/镜像及现场证据；继续工作前核对
