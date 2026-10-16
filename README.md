# Measix Runcove

Runcove 为 MCP 客户端提供持久工作空间，包含命令与文件工具、浏览器、云桌面、文件交换和临时 Web 发布。同一 Rust 核心支持两种部署方式：

| 部署方式 | 适用场景 | 隔离与管理 |
|---|---|---|
| **Runcove Server** | 企业或外部平台管理多用户空间 | microsandbox VM 隔离；提供管理 API，Docker 宿主需要 KVM |
| **Runcove Standalone** | 个人、自托管单空间 | 一个使用者、一个 Docker 容器、一个空间；直接运行本地进程，无 MSB/KVM 或管理 API |

**Runcove Workspace** 是 Server 的桌面 VM rootfs，包含浏览器和工具依赖，自身不包含 MSB。它与 Standalone 共用依赖构建来源；两种入口共用工具、任务、控制权和文件实现。

一个空间可连接多个已认证客户端和桌面观看者，人工输入使用同一控制租约。当前镜像资格平台为 Linux/amd64；ARM 和目标 NAS 需单独验证。

## microsandbox 与项目定位

[microsandbox](https://github.com/superradcompany/microsandbox) 是面向不可信代码、Agent 和自动化任务的本地优先 microVM 运行时，提供 CLI、SDK 和 VM 生命周期、命令、文件及存储能力。项目介绍与使用方式见[官方网站](https://microsandbox.dev/)和[官方文档](https://docs.microsandbox.dev/getting-started/introduction)。

Runcove Server 使用 microsandbox 作为空间托管引擎，在其上提供持久用户空间、远程访问与桌面工作台。Tuck 等 MCP 客户端使用这些空间时，VM 由 Server 层管理；平台接入关系见[接入参考](docs/server-api.md#5-measix-control-hub-接入)。Standalone 则复用本项目能力，直接运行在单个容器中，不使用 microsandbox。Server 当前锁定的版本、临时补丁及升级门槛见[运行时维护](docs/runtime.md)。

本项目侧重把运行时能力组织成可直接交付的工作空间：

- **持久空间与用户管理**：固定空间身份、根盘和工作卷，统一管理 MCP/DAV 凭据、禁用、删除及资源回收；客户端无需自行编排 VM。见[管理 API](docs/server-api.md)。
- **浏览器、桌面与人工协作**：自动化和远程桌面共用一个可见浏览器，配合任务期限、取消清理、人工接管及休眠恢复；工作台文件传输与基础工具可独立使用。见[浏览器与云桌面](docs/browser-desktop.md)。
- **直接交换工作文件**：MCP、工作台、DAV 与发布读取同一工作区；大文件走 HTTP 流，DAV 有独立凭据与原子上传保护。4 GiB 完整读取、Range、取消及写满验证的范围见[验证记录](docs/validation-records.md)。
- **可核对的部署与维护**：提供 Server/Standalone 两种入口、Compose 和离线包；固定构建来源、补丁及实际程序身份，并保留升级、冷备恢复和真实桌面/文件验收。见[部署](docs/deployment.md)与[运维](docs/operations.md)。

### 相关项目

以下按 **2026-10-16** 核对的项目文档概括定位，链接固定到当时版本；没有做跨项目运行测试或性能排名。

| 项目 | 公开定位与能力 | 与 Runcove 的侧重点区别 |
|---|---|---|
| [microsandbox-mcp（官方）](https://github.com/superradcompany/microsandbox-mcp/blob/ff60d5328dd5c7162e45d88e317e260e1b11ea66/README.md) | 通过 stdio MCP 将 SDK 的沙箱创建、执行、文件、卷、快照及监控能力交给 Agent | 适合直接驱动运行时；Runcove 侧重已分配的持久空间、用户级远程 HTTP 访问和配套工作台 |
| [Holm（第三方）](https://github.com/holm-inc/holm/blob/2590c3b62f32ad3978231fd0397943097d7ce0b1/README.md) | 支持容器/VM 与多种后端的桌面控制，含观看、接管、REST 和 HTTP MCP；提供可选 microsandbox 集成 | 桌面与接管能力存在重叠；Runcove 聚焦两种部署入口、用户空间生命周期、DAV 文件交换及成套离线交付 |
| [agent-sandbox（第三方）](https://github.com/ErRickow/agent-sandbox/blob/994c060f5bbbeb99a8791fd83d8e65337876f395/README.md) | 以统一运行接口和 skills 适配 E2B、Vercel、Daytona、Modal、microsandbox，提供命令与 agent-browser 能力 | 适合在多个 provider 间复用任务；Runcove 当前聚焦自托管空间及其浏览器、文件和管理合同 |

选型取决于需要运行时工具、跨后端适配，还是完整的托管工作空间。上面的能力组合是本项目的产品取舍，不表示相关能力为本项目独有，也不表示其他项目缺少相应扩展。

## 开始使用

准备 Linux Docker；Server 另需 `/dev/kvm`。从源码构建个人单空间版本：

```bash
cd agent-space
bash docker/build-standalone.sh
```

随后按[部署指南](docs/deployment.md#独立单空间构建与启动)设置构建输出的镜像标签和访问凭据，使用仓库 Compose 启动。桌面入口为 `/desktop/`，MCP 地址为 `/mcp`；外部 DAV 默认关闭。Server 的构建、首次状态初始化及多用户接入也从[部署指南](docs/deployment.md)开始。

每次部署只使用一份 TOML 配置。Server 持久卷保存账号状态、MSB 数据库及空间双盘；Standalone 持久化 `/workspace`，用户数据使用 UID/GID1001。既有服务升级、回滚及工作区转移见[运维手册](docs/operations.md)。

## 文档

| 阅读目的 | 文档 |
|---|---|
| 安装、配置、Compose 和离线包 | [部署指南](docs/deployment.md) |
| 升级、备份、迁移及故障恢复 | [运维手册](docs/operations.md) |
| Server 用户、管理 API 和平台集成 | [Server 接入参考](docs/server-api.md) |
| 浏览器工具、任务、桌面控制和文件面板 | [浏览器与云桌面](docs/browser-desktop.md) |
| 外部文件客户端、认证及文件语义 | [WebDAV](docs/webdav.md) |
| 临时文件、网站和端口发布 | [Web 发布](docs/web-publish.md) |
| 共享核心、状态、生命周期与受管进程 | [架构](docs/architecture.md) |
| 源码开发、测试、发布及文档维护 | [开发指南](docs/development.md) |
| Server 的 MSB 来源、补丁及升级门槛 | [运行时维护](docs/runtime.md) |
| 固定版本实测和未验证边界 | [验证记录](docs/validation-records.md) |

文档描述当前实现；验证记录绑定自己的源码、镜像及日期，不代表实时服务状态。公开 Git 历史从整理后的首次提交开始，不包含此前的私有开发记录；仍有效的结论已并入现行手册。

## 参与开发

Rust workspace 位于 `agent-space/`，代码职责与检查命令见[开发指南](docs/development.md)，Agent 工作约束见 [AGENTS.md](AGENTS.md)。提交问题时注明部署目标、平台、源码或镜像身份、复现步骤和脱敏错误；行为变更同步对应参考文档及验证范围。

目前提供中文文档；后续双语入口及目录约定见[文档维护](docs/development.md#文档维护)。

## 致谢

感谢 [microsandbox](https://github.com/superradcompany/microsandbox) 的维护者和贡献者提供 Server 的 microVM 基础，以及对问题复现、反馈和修复的持续投入。感谢其依赖的 [libkrun](https://github.com/containers/libkrun) 与 [smoltcp](https://github.com/smoltcp-rs/smoltcp) 社区。本项目维护空间服务与集成层，底层虚拟化和网络基础来自这些上游项目；本地补丁、已反馈问题与撤销条件公开记录在[运行时维护](docs/runtime.md)。
