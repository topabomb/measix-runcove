# Measix Runcove Docker 部署与验证

本页说明 Server 与 Standalone 的部署、配置、备份和离线包使用。产品与镜像职责见[仓库首页](../README.md)，源码开发和完整发布资格见[维护指南](development.md)；确切交付版本以镜像报告为准。

| 镜像职责 | 部署边界 |
|---|---|
| Runcove Server | 托管多空间服务，携带 MSB；Docker 宿主需 KVM |
| Runcove Workspace | MSB VM 使用的空间 rootfs，自身不包含 MSB；可由 Server 离线携带 |
| Runcove Standalone | 一个容器直接运行一个使用者的一个空间，无 MSB/KVM 依赖；不支持配置多个空间 |

镜像资格平台为 Linux/amd64；Server 锁定 microsandbox 0.7.2 并需要宿主 `/dev/kvm`。管理与外部集成合同见[接入参考](server-api.md)。

每次部署只有一个生效的 TOML 文件；示例是模板，环境变量只提供凭据值。配置不热加载，未知字段、错误类型和冲突的新旧名称在启动前拒绝。

| 职责 | Server | Standalone |
|---|---|---|
| 入口与协议 | `[server]`：8932、公共 origin、协议空闲期限 | 同样的入口设置，协议期限默认 0 |
| 空间容量 | `server.max_running_spaces=2`；与 MCP 连接数分开 | 固定一个空间，不接受此项 |
| 资源与执行 | `[workspace]`：新 VM 镜像/2 CPU/3072 MiB/8192 MiB 工作盘/512 MiB tmpfs，以及执行限额 | `[execution]` 只负责 Bash/30 秒/2000 行/51200 bytes；资源交给 Compose |
| 生命周期 | `[lifecycle]` 的请求/工作空闲期限及检查间隔 | 无 VM 生命周期配置；桌面休眠沿用共同合同 |
| 身份与状态 | `[state]` 唯一 state.json；`[admin]` 引用管理员环境凭据 | `[auth]` 引用单实例凭据；没有持久账号表 |
| 文件与发布 | `[webdav]` 默认开启且独立凭据；`[web_publish]` 提供有限 VM 保活 | DAV 默认关闭且需独立凭据；发布不控制容器停机 |
| 日志 | `[logging]` | `[logging]` |

资源与输出限额必须为有效正数；Server CPU 输入不能在 u8 转换时截断。新默认不覆盖已有 VM、磁盘、凭据或固定镜像。旧配置转换会保留其有效值，新格式再次转换也保持默认值，操作见[运维手册](operations.md#schema3-配置转换)。

## 1. 构建

在 `agent-space/` 目录执行：

```bash
cargo check --workspace --all-targets --locked
bash docker/build.sh
```

脚本按 `Cargo.lock` 校验 SDK/运行时，按源码内容构建 `runcove-server:runcove-<摘要前16位>`；不再覆盖 `0.7.2` 或 `latest`。应用版本与 SDK 版本分开记录，镜像 ID 是验收依据。下文 `NEW_IMAGE` 须替换为本次验证通过的标签。镜像包含服务端、三个受管helper、中英文桌面前端、msb CLI、libkrunfw、默认自定义及锁定桌面沙盒镜像，不包含用户配置或数据。

Server 的服务与实际 VM CLI 使用配对的 MSB 生命周期补丁和双端 SQLite 3.51.3，构建及启动核验固定输入、补丁和最终程序身份。来源、上游状态与逐项撤销门槛集中在[运行时维护](runtime.md)，升级前核查；Agent Space 不直接修改 MSB 表，生产运行期间不要另用 CLI 修改同一数据库。

### 独立单空间构建与启动

```bash
bash docker/build-standalone.sh
# 用上一步输出的确切标签替换 RUNCOVE_IMAGE；仅为本实例配置一份 TOML。
export RUNCOVE_IMAGE=runcove-standalone:runcove-BUILD_DIGEST
export RUNCOVE_TOKEN='REPLACE_WITH_AT_LEAST_32_RANDOM_CHARACTERS'
docker compose -f docker/compose.standalone.yaml up -d
```

`build-standalone.sh` 复用 Workspace 桌面依赖构建，用经 image ID 核对的本地内容标签作为基础，不要求先推送 registry。镜像内 `/opt/agent-space/release.json` 记录目标、构建输入、匹配 workspace、helper 和 UI 摘要；资格报告绑定实际 image ID。当前固定资格为 Linux/amd64；无 KVM 不等于已经支持 ARM64 或所有 NAS。

Compose 示例读取 `docker/configs.example/standalone.toml`，持久化 `/workspace`，限制 CPU/内存/shm/tmpfs，并提供浏览器所需的 seccomp 配置。首次只初始化空工作卷根目录；已有数据的 UID/GID 不会被递归改写。数据进程使用1001:1001；导入数据先按运维手册显式映射。root 服务配置和访问凭据不能放进用户工作卷。

访问 `/desktop/` 并使用该实例 Token 登录，MCP 地址为 `/mcp`，发布为 `/pub/{name}/`。只有一个使用者的一个空间，无账号/空间选择和管理 API。访问凭据通过环境变量提供，TOML 只保存环境变量名称；重建后必须继续提供凭据。外部 DAV 默认关闭；启用时配置 `[webdav] enabled=true`、独立 `RUNCOVE_DAV_TOKEN` 并在编排中映射8933端口，入口为 `/dav/`。工作台文件面板不依赖外部 DAV 开启。

独立 TOML 支持 `[server]`、`[auth]`、`[execution]`、`[logging]`、`[webdav]`、`[web_publish]`；不接受用户/空间清单、MSB、VM 容量/生命周期或管理配置。执行默认 Bash、30秒、2000行/51200bytes；监听默认8932，MCP协议会话默认不因空闲过期。DAV 的 `public_port` 是 Server 管理链接投影设置，不在独立配置中提供。

正常 TERM 会取消 MCP 长连接、DAV 与现有任务/helper。若桌面 root owner 被强杀，状态保守 BLOCKED 并要求 `docker compose ... restart`；基础命令和文件继续可用。不要用 `pkill` 或删实例记录强行恢复。使用者安装到容器根文件系统的软件不承诺跨换镜像保留。

### 托管版配置与 Compose

`docker/compose.server.yaml` 只适用于 Server：KVM、数据/日志卷、独立 DAV 端口及管理员环境凭据。首次使用相同镜像/配置/数据卷执行 `init-state` 后再启动服务；后续升级不能重复初始化。

Server 的新默认监听8932、`server.max_running_spaces=2`，生命周期字段为 `request_idle_timeout_secs` 和 `work_idle_timeout_secs`。旧字段仍兼容但冲突会报错；旧 `[engine] type="msb"` 可由转换器内化。每个空间的镜像/VM默认只影响新建空间，不能通过修改默认重建旧盘。旧配置转换步骤见运维手册。

## 2. 配置与首次启动

下面的 `docker run` 示例使用 `docker/configs/config.toml`。仓库 Compose 则分别挂载 `docker/configs.example/config.toml` 或 `standalone.toml`；使用 Compose 时先将部署文件复制到自己的部署目录，编辑其中实际挂载的那个文件。这两种启动方式择一，避免修改一份配置却运行另一份。

```bash
mkdir -p docker/configs
cp docker/configs.example/config.toml docker/configs/config.toml
```

开发默认将 `server.public_base_url` 和 `webdav.public_base_url` 留空。链接从实际请求入口生成，本机Core使用它所在环境可达的稳定本机地址，局域网客户端使用当前宿主IP；更换局域网无需改IP配置或重启容器。已缓存或交付的旧绝对链接不会自动更新，客户端应重新获取或换成当前入口。Core内部连接不要被面向其他客户端的公共链接覆盖。

服务监听默认 `0.0.0.0`。主服务和DAV使用独立监听器，示例开发配置：

```toml
[server]
listen = "0.0.0.0:8932"
public_base_url = ""

[webdav]
enabled = true
listen = "0.0.0.0:8933"
public_port = 18933
public_base_url = ""
```

对应Docker端口为 `-p 0.0.0.0:18932:8932 -p 0.0.0.0:18933:8933`。`public_port` 是生成DAV链接所需的宿主端口，容器无法自动推断Docker映射；端口固定后换网络无需修改。上述配置片段需与完整动态用户配置合并，不能作为完整配置替换。

**正式部署必须明确设置两个稳定的HTTPS公共origin**（例如 `server.public_base_url = "https://workspace.example.com"`、`webdav.public_base_url = "https://files.example.com"`），并将网关分别转发到8932和8933，保留Host及 `/u/{username}/...` 路径。公共基址用于固定链接及识别域名入口，不作为拒绝本机/局域网访问的唯一白名单。空配置只开放localhost、回环和私有/链路本地IP入口；不自动信任任意域名、公网IP或转发头。局域网HTTP不加密凭据，正式远程部署使用HTTPS。

将 `state.dir` 放在持久卷 `/var/lib/microsandbox` 下，保留 `[admin].token_env = "AGENT_SPACE_ADMIN_TOKEN"`。默认范例使用动态用户模式，不含内置账号；首次启动无需 `[users]`。旧静态用户配置如仍需使用，须显式设置 `[webdav].enabled = false`。Web发布默认启用，使用主服务入口及 `/u/{username}/pub/{name}/`，仅发布可信内容；DAV默认开启。Token、用户/空间、路径、浏览器Origin/CSRF校验与入口地址配置分开负责。主服务端口不提供DAV，DAV端口不提供管理/MCP、桌面及发布；两个端口均有健康检查。

用独立的长随机秘密设置管理员 Token，并在**同一配置和数据卷**上先初始化状态，再启动服务：

```bash
NEW_IMAGE=runcove-server:runcove-REPLACE_WITH_BUILD_INPUT
export AGENT_SPACE_ADMIN_TOKEN='replace-with-a-long-random-secret'
docker volume create agent-space-data
docker run --rm \
  -e AGENT_SPACE_ADMIN_TOKEN \
  -v agent-space-data:/var/lib/microsandbox \
  -v "$PWD/docker/configs/config.toml:/etc/agent-space/config.toml:ro" \
  "$NEW_IMAGE" init-state
docker run -d \
  --name agent-space --restart unless-stopped \
  --device /dev/kvm --stop-timeout 60 \
  -p 0.0.0.0:8932:8932 -p 0.0.0.0:8933:8933 -e AGENT_SPACE_ADMIN_TOKEN \
  -v agent-space-data:/var/lib/microsandbox \
  -v "$PWD/docker/configs/config.toml:/etc/agent-space/config.toml:ro" \
  "$NEW_IMAGE"
```

```bash
docker inspect agent-space --format '{{.State.Health.Status}}'
docker exec agent-space curl -fsS http://127.0.0.1:8932/healthz
docker logs agent-space
```

正常启动不会在状态文件缺失时自动创建空账号库。服务 `/healthz` 会读取状态和 SDK 后端元数据；失败返回 503，Docker 健康检查变为 unhealthy，但 Docker 不会仅因此自动重启容器。排查时保留卷现场，不直接删数据库、WAL 或磁盘。

Docker Desktop 下通过 WSL 重建时，bind source 使用 WSL 可访问的原文件路径（例如 `/path/to/deployment/config.toml`）。不要把 inspect 中已解析的 `/run/desktop/mnt/host/...` 路径直接交回 WSL 接口；启动前确认容器内目标是只读普通文件，并与原配置比较内容摘要。数据/日志卷仍按原卷名复用，不能把配置路径错误当作状态丢失而重新初始化。

如果现有部署出现 `database disk image is malformed`，先按 [故障恢复手册](operations.md#sqlite-故障处置) 冷备份、验证副本并受控替换。旧版本修复与账号迁移各有适用来源，按运维手册分别核对。

桌面预设可设 `[workspace].image={preset="runcove-desktop"}`，原字符串保持兼容；schema3固定每个用户镜像。动态schema2须停止旧服务、冷备份后显式migrate-state，步骤见[运维手册](operations.md)。已有根盘不会随默认值热换。

已有部署的状态迁移、完整冷备、故障恢复和回滚统一见[运维手册](operations.md)。Server 的数据卷整体保存 state、MSB DB/WAL/SHM、根盘及登记卷；Standalone 的 `/workspace` 单独备份，停止浏览器后保留 numeric UID/GID、权限和链接。两种部署都应同时保存实际配置和凭据来源；不要执行 `docker compose down -v` 删除业务数据。

## 3. 验证与离线包

源码检查、真实场景矩阵和最终镜像资格命令统一见[开发与发布指南](development.md)。共同核心同时验证两目标；Server 还需生命周期和精确旧生产镜像升级/完整冷备回滚。开发二进制绑定测试不能作为最终镜像资格，成功清理是门槛的一部分。

完成资格后，将各脚本输出的确切报告路径赋给下面的变量，再从源码 `agent-space/` 打包：

```bash
python3 docker/package.py --target server \
  --verification "$SERVER_DESKTOP_REPORT" --files "$SERVER_FILES_REPORT" \
  --storage "$SERVER_STORAGE_REPORT" --upgrade "$SERVER_UPGRADE_REPORT" \
  --output /path/to/releases
python3 docker/package.py --target standalone \
  --verification "$STANDALONE_REPORT" --files "$STANDALONE_FILES_REPORT" \
  --storage "$STANDALONE_STORAGE_REPORT" --output /path/to/releases
```

打包器核对实际镜像、当前输入摘要、完整共同场景、4 GiB DAV/截短/取消、20 次写满和拒绝上传的固定截止，Server 另核对升级回滚。Standalone 还要求产物枚举不能经用户链接越过数据 UID 权限。归档内 image/config 身份、release 元数据与报告必须相符。应用版本使用构建摘要；状态 schema3 不是应用版本。

包只包含镜像、匹配 Compose/配置模板、seccomp（Standalone）、迁移工具、长期文档、引用来源及固定证据，不包含用户数据、现场配置或访问秘密。文档复制保持相对链接，不绑定一次实验日期。解包后：

```bash
sha256sum -c SHA256SUMS
sh load.sh
# 按目标提供凭据，编辑 Compose 实际挂载的唯一配置文件。
# Server 在同一镜像/配置/数据卷上先 init-state；已有状态不得再次初始化。
docker compose up -d
```

加载器先校验归档摘要，再加载并核对精确 image/config 和源码身份，给 `.env` 使用的内容版本标签赋值；拒绝覆盖指向其他字节的已有内容标签。经典 Docker 与 containerd 存储可能使用不同本地 ID，归档 config 摘要仍须一致。发布前用包内 Compose 实际初始化/启动并核对目标接口，不能只检查 YAML。

只有来源确为旧静态发布包时才另行运行 `verify-static-upgrade.py`；它不替代当前 Server 的生产版本升级回滚资格。异常重启/DAV 按需恢复的定向入口为 `tests/test_dav_restart_vm.py`，只允许新建独立 Host/卷，禁止指向日常服务。脚本覆盖 crashed + PID 复用、并发首次 PROPFIND、暂停恢复、正常重启及有界故障；原 sandbox、双盘、配置和凭据均须保持。

协议版本与请求方式见[接入参考](server-api.md#4-mcp-工具和-web-发布)。局域网 DAV 映射需显式设置 `webdav.public_port`；正式入口使用稳定 HTTPS origin，主服务和 DAV 分别转发至 8932/8933。资格、离线包和实际部署范围见[验证记录](validation-records.md)。生产替换仍需维护窗口和完整冷备，镜像通过资格不会自动切换服务或推送 registry。

## 4. 可选出站代理

Server 镜像含 sing-box 和 TUN 依赖；检测到容器内 `/etc/agent-space/sing-box/config.json` 时启动。首次部署后进入容器手工配置并重启生效；重建容器须单独保留并恢复该容器内配置。TUN 还需 `--device /dev/net/tun --cap-add NET_ADMIN`。工作区原生 SOCKS 出站代理可用 `[workspace.outbound_proxy]`；密码通过宿主环境变量引用。

Server 的 `[workspace].max_tcp_connections` 与 `max_udp_connections` 直接传给 SDK；省略保留 SDK 默认，显式 0 表示不限。网络限额和代理仅影响按该配置创建的 VM，不通过修改部署默认值重写已有空间。
