# 运维、迁移与故障恢复

适用于现有部署的升级、完整冷备回滚和工作区转移。首次安装见[部署指南](deployment.md)，固定镜像及现场证据见[验证记录](validation-records.md)。下列源码命令在 `agent-space/` 执行；操作前重新核对目标 Docker context、实际镜像 ID、挂载、配置与空间身份。

| 来源与目的 | 路径 | 保留边界 |
|---|---|---|
| 当前 schema3 Server → 新 Server | 本页当前 Server 升级 | 保留账号、凭据、空间/资源身份及两盘；通常只需转换旧配置 |
| 动态 schema2 / 静态用户 → Server | 本页历史状态迁移 | schema2 保留凭据；静态导入另行签发，不可混用 |
| 一个 Server 工作区 → Standalone | 本页选定托管空间转移 | 只转移工作区文件/profile，明确 UID/GID 映射；目标使用新凭据 |
| 文件备份 → 已开通新空间 | 本页完整工作区恢复 | 保留目的空间身份，留存旧暂存树并建立新 owner |
| 服务故障 → 冷备恢复 | 本页回滚与 SQLite 故障处置 | 先保留现场，只在独立副本检查、独立新卷恢复 |

## 当前 Server 升级

1. 只读记录实际服务镜像 ID、runtime/worker 身份、端口/挂载、配置摘要和各空间的 ID、凭据摘要、sandbox、登记卷、固定镜像及待清理意图；不输出明文凭据。核对 sing-box 等容器内独立配置的实际保存位置。
2. 在隔离容器/卷上用精确旧生产镜像建立真实空间，写根盘与工作盘标记、浏览器 profile/标签和文件。使用正常签发的原 MCP/DAV/管理员凭据作为升级前后测试输入，不把它们写入证据。
3. 正常停止隔离源，冷备完整 MSB 数据/应用状态、日志和配置；验证归档、摘要和独立数据库副本。旧配置转换先预览，再输出新文件，不覆盖原文件；保留 CPU/内存/盘/tmpfs、运行容量、超时、网络和 origin 值。
4. 必要状态迁移使用排他锁、完整预检、精确旧文件备份和原子提交。保留原 token/keyId、空间/资源身份、固定镜像和 cleanup 意图；重复执行无变化。正式 schema 不变时不做无意义迁移。
5. 候选 Server 挂载从冷备恢复的隔离新卷启动，验证管理 API、原 MCP/DAV、真实桌面首帧与控制、文件/发布和重启恢复；核对身份及两盘/profile 内容不变。错误配置、提交故障和中途失败分别验证不发生部分提交。
6. 停候选；把完整旧冷备恢复到另一套新卷，以原镜像 ID/原配置启动并验证原凭据、两盘和真实 MCP/DAV/桌面。不得将旧程序直接挂到已经升级的卷。
7. 隔离演练通过后，在维护窗口对实际部署重新执行冷备、配置转换及切换，绑定同一已验证目标镜像；实际生产切换单独记录，不用演练结果代替现场验收。

### schema3 配置转换

旧配置有 `[engine]`、`max_sessions` 或旧生命周期字段时，兼容加载保留旧隐式监听3100、容量12。新配置使用统一8932和容量2。迁移须显式转换并检查原有效值，不能把新安装默认套进生产。

```bash
agent-space-mcp --config /path/old.toml convert-config --dry-run
agent-space-mcp --config /path/old.toml convert-config --apply --output /path/new.toml
```

预览只返回摘要和变更状态，不输出秘密。apply 在相同目录原子发布0600新文件，拒绝覆盖源文件、已有目的文件及并发创建的目的文件；错误/冲突/未知字段会在提交前失败。schema3 状态结构保持，不做无收益的schema4迁移或凭据轮换；新增状态校验拒绝活动账号带cleanup、删除账号缺少cleanup等不合法组合。

### 选定托管空间 → Standalone

这是工作区迁移，不是账号库或 VM 克隆。只导入明确选定的一个空间；不自动将企业多用户转为独立多空间。

停止源空间写入并优雅关闭 Chromium，导出 `/workspace`，源归档/清单保留普通文件内容、UID/GID/mode 和符号链接；核对归档和逐文件清单。源 VM/卷与凭据原样保留。独立目标使用新配置和访问凭据，不导入 MSB 数据库、根盘、旧账号状态、PID 或临时任务。目标用户数据副本明确将 root:root 映射为 UID/GID1001，原1001保留；其他所有者预检报告，禁止猜测映射或跟随链接全盘 chown。记录目标元数据映射，源归档保持原值。内部暂存依本页完整工作区恢复规则留存并处理 owner 归属，不改写旧归属标记。先在隔离 Standalone 验证工作文件、profile/标签、root-owned 0755目录/0644文件与混合所有者、真实文件流、接管和容器重建；同时证明源未被修改。容器根盘中用户安装的软件不在此迁移保证内。

工作区转移使用 `agent-space/docker/workspace-transfer.py`：源先停止写入并以只读卷访问，导出归档内含逐文件摘要/UID/GID/mode/链接清单；导入先预检，apply 只原子发布全新目录，不能与已有目标合并。

```bash
python3 docker/workspace-transfer.py export --source /workspace --archive /backup/workspace.tar
python3 docker/workspace-transfer.py import --archive /backup/workspace.tar --destination /srv/runcove-import/workspace
# 核对摘要和 unsupportedOwners 后，在隔离目标以root执行：
python3 docker/workspace-transfer.py import --archive /backup/workspace.tar --destination /srv/runcove-import/workspace --apply
```

上述脚本命令从源码 `agent-space/` 执行；离线包的脚本位于包根 `workspace-transfer.py`，使用时相应调整脚本路径。

apply 的目标父路径及祖先必须是真实、root拥有且其他用户不可写的目录（root拥有的sticky临时目录除外），防止提取时被置换。stage 在全部落盘、校验和fsync完成前保持root:root0700，目录mtime最后恢复，发布后通过固定根句柄赋予1001。只支持完整 `(0,0)`→`(1001,1001)` 和 `(1001,1001)`保留；包括 `(0,1001)` 在内的其他组合只报告，不猜测。旧暂存被保留到新 `.agent-space/restored-runtime-<随机ID>/`，新实例建立自己的owner。预览和 apply 都校验归档后的路径及新增父目录；例如源 `.agent-space` 是链接而旧暂存需要归入其中时，必须在写入前报错，由维护者明确处理源目录冲突。

## 历史状态迁移

### 动态 schema2 → schema3

使用新二进制/新容器、旧数据卷和旧配置，先预览：

```bash
agent-space-mcp --config /path/config.toml migrate-state --dry-run
# 确认数量和备份后，仍保持旧服务停止：
agent-space-mcp --config /path/config.toml migrate-state --apply
```

Docker 对应命令：

```bash
docker run --rm --device /dev/kvm \
  -e AGENT_SPACE_ADMIN_TOKEN \
  -v agent-space-data:/var/lib/microsandbox \
  -v /absolute/old-config.toml:/etc/agent-space/config.toml:ro \
  NEW_VERIFIED_IMAGE migrate-state --dry-run
# 使用同样挂载和配置，最后参数换成 migrate-state --apply
```

新容器入口沿用挂载数据卷的运行时元数据，不创建VM。命令持有与正常服务相同的排他状态锁，覆盖镜像读取、验证、备份及提交；旧服务仍持锁时失败。dry-run不写状态或备份。apply用create_new写精确 `state.schema2.backup.json`，fsync后原子替换state.json；已有该备份不覆盖。解析、镜像读取或锁失败保持原状态文件。

迁移后每个旧用户的 `image.kind=custom`；实际reference来自原VM或旧默认值。username、agentSpaceId、status、credential/davCredential、sandboxName、ownedVolumeNames和cleanup均保持。schema3正常启动要求每条记录有明确image；不静默猜测旧格式，不自动把标签升级为当前digest。

### 旧静态用户 → schema3

#### 支持的静态发布基准

静态源镜像 ID 为 `sha256:1c389b9be5db3aa7d397375de1eac8a79839de0cd8b7e828b5c010ef325f8b72`（历史标签 `agent-space:0.7.2`）。其配置为 `[users]`、旧 `/mcp` 入口，仅有 bash/read/write/edit 四工具；不包含动态管理、DAV 或目录/端口发布。必须按实际 ID 核对，不凭可变标签推定来源。固定验证见 [SQLite 旧版恢复证据](evidence/sqlite-20260930/legacy-repair-results.json)。

#### 导入步骤

停止旧容器并备份整个数据卷。迁移配置临时同时保留旧 `[users]` 和新增 `[state].dir`，然后用**目标新镜像**、原卷和该配置依次执行：

```bash
docker run --rm -v agent-space-data:/var/lib/microsandbox \
  -v "$PWD/docker/configs/config.toml:/etc/agent-space/config.toml:ro" \
  "$NEW_IMAGE" init-state
docker run --rm -v agent-space-data:/var/lib/microsandbox \
  -v "$PWD/docker/configs/config.toml:/etc/agent-space/config.toml:ro" \
  "$NEW_IMAGE" import-users --dry-run
docker run --rm -v agent-space-data:/var/lib/microsandbox \
  -v "$PWD/docker/configs/config.toml:/etc/agent-space/config.toml:ro" \
  "$NEW_IMAGE" import-users --apply
```

先核对 dry-run 列出的用户、sandbox 和卷归属，确认后才 apply。导入不启动或删除 VM，也不导入旧 Token。移除 `[users]` 后，以同一卷启动新服务，通过管理 API 为每位用户签发新 Token 并更新客户端；运行配置不能同时启用 `[state].dir` 和 `[users]`。已初始化的状态目录不要再次运行 `init-state`。支持的旧版本迁移基准是上文记录的静态 `[users]` 发布包，不保留未发布动态状态中间版本的迁移命令。升级后 MCP 入口为 `/u/{username}/mcp`，DAV 入口为独立文件域名下的 `/u/{username}/dav/`，按管理 API 返回的 URL 配置客户端。


### 历史空间启动及新增桌面空间

1. 用新包和schema3数据启动，核对`/healthz`、Admin用户列表/资源、MCP原工具、DAV Range/上传及现有文件哈希。确认原 custom 空间仍显示五工具，根盘/卷ID保持。
2. 可将部署默认image改为 `{preset="runcove-desktop"}`，或Admin创建时单独传 `image:{preset:"runcove-desktop"}`。两者只作用于**新空间**。桌面模板为2vCPU/3072MiB/8GiB工作盘/512MiB tmpfs，明确配置不会被预设暗改。
3. 新空间调用browser_snapshot懒启动，访问 `/u/{username}/desktop/`，用该用户MCP Token登录。验证AI/人工互斥、无动作60秒归还、300秒休眠唤醒、文件交换及多人观看。
4. 现有空间如需桌面，创建另一个明确预设的新空间，以DAV/既有文件工具迁移业务文件。不要给原根盘直接改kind或reference，不从实验空间复制登录profile/Token，不热换镜像。

## 将完整工作区备份恢复到新空间

这是将业务文件迁入已开通新空间的操作。整套服务回滚使用[完整数据卷冷备](operations.md#备份与回滚)。默认业务迁移仍遵循上一节；完整备份中的桌面 profile 仅在用户明确要求完整恢复时保留，并另行验证镜像兼容性。

1. **核对备份集**：多来源合并到同一个 `workspace/` 根目录，先检查同名文件、文件/目录类型及符号链接冲突；有差异就明确处置，不静默覆盖。记录归档 SHA-256、文件数、逐文件摘要及路径映射。ZIP 的 Unix mode 属性可保存权限和链接类型，但不保证包含 UID/GID；同时保留源元数据清单，或另存带 numeric-owner 的 tar，恢复时使用源记录还原所有者。文件摘要与元数据应来自同一份停止后的工作盘或一致快照。Windows／WSL 间传输归档使用 scp 或二进制文件/管道接口，避免 Windows PowerShell 5 文本管道转接 tar／ZIP；两端均核对归档摘要。
2. **确认目的空间**：核对 username、agentSpaceId、sandboxName、ownedVolumeNames、镜像及可用容量；开通账号只登记身份，VM/盘为 `notfound` 时先通过正常 MCP／DAV／桌面请求初始化。记录账号状态和两类凭据摘要。维护停机使用正常服务关机；禁用或删除账号会撤销凭据，不能代替临时停机。
3. **停服恢复文件**：确认 VM 已正常退出且没有其他运行实例挂载目的卷，完整留存目的工作区现有文件后只恢复 `/workspace`。不导入旧根盘、state.json 或 MSB 元数据库。解包前检查路径归属，拒绝绝对路径、`..`、重复条目和通过符号链接父目录写出目标根；同时防止目的树的既有符号链接影响解包。保留隐藏文件、源 UID/GID、权限及链接目标，并在服务再次运行前逐个核对源文件摘要和元数据。
4. **处理旧暂存归属**：新空间 ID 不同于备份来源时，检查 `.agent-space/uploads/owner` 及旧 `.agent-space-upload/owner`。归属不匹配会导致 `dav_helper_start: Forbidden`，VM Running 或 `/healthz` 成功仍不能证明 DAV 可用。确认旧实例不在目的工作盘运行后，完整留存这两个目录，例如选择尚不存在的 `.agent-space/restored-runtime-<date>/uploads`、`legacy-uploads` 作为目的路径；保留其中的部分上传、标记和原始权限，不改写旧 owner、不按目录名删除、不让新清理器接管。在恢复文件核验完成后、对外恢复请求前处理，由正常 DAV 请求创建新空间自己的暂存区。
5. **验证后交接**：核对原新空间身份、凭据摘要及实际工作盘容量；正常请求须能按需恢复，DAV `PROPFIND Depth:1` 返回 207，抽样 GET 内容摘要一致，再通过实际 Orchelm／Tuck 文件入口列目录并读取文件。重启后再校验全部源文件；旧暂存使用已记录路径映射，运行时 profile 的合理变动需单独说明。保留权威备份和恢复证据，通过后清理本次临时 helper、脚本及清单。独立资格 Host 的桌面截图不替代目的账号的登录/控制验收，未运行项明确记录。

真实完整备份恢复及旧暂存冲突的验证见[2026-10-14 生产记录](validation-records.md#工作区恢复记录2026-10-14)。

## 备份与回滚

Server 的 state、MSB DB/WAL/SHM、根盘、登记工作卷、镜像缓存和运行库副本须按同一停机时点整体备份；另存日志、实际 TOML、启动参数及容器内独立配置（如 sing-box）。Standalone 停止浏览器和容器后，完整备份 `/workspace`，保留 numeric UID/GID、mode 和链接；配置及凭据来自部署来源，不能只保留数据卷。

下面仅演示 **Server 数据卷** 归档。在实际 Docker 主机执行，事先将 `CONTAINER`、`DATA_VOLUME`、`VERIFIED_IMAGE` 和绝对 `BACKUP_DIR` 设置为核对过的目标；备份放到容量足够的私有外部存储、新目录，配置/日志等其他挂载另外归档。停止后须先核对 VM 已正常退出且无共享写入者，再运行 tar 归档：

```bash
set -euo pipefail
umask 077
mkdir "$BACKUP_DIR"
docker stop --time 60 "$CONTAINER"
test "$(docker inspect "$CONTAINER" --format '{{.State.Running}}')" = false
test "$(docker inspect "$CONTAINER" --format '{{.State.Pid}}')" = 0
docker run --rm --entrypoint /bin/tar \
  -v "$DATA_VOLUME:/data:ro" -v "$BACKUP_DIR:/backup" \
  "$VERIFIED_IMAGE" --sparse --numeric-owner -czf /backup/data.tar.gz -C /data .
tar tzf "$BACKUP_DIR/data.tar.gz" >/dev/null
(cd "$BACKUP_DIR" && sha256sum data.tar.gz > data.sha256)
```

容器停止只是第一项核对：还须确认 VM 正常退出、没有其他共享写入者；强杀/超时或未知 run 不能当作正常关机。保留每个挂载的归档/摘要、精确 image ID、原账号/凭据来源、双盘摘要及状态基线，验证完整备份可恢复到隔离新卷后才进行版本切换。完整 Docker inspect 环境可能含秘密，只能在受保护的运维存储中保留，公开证据不包含明文。

回滚前停止候选并保存升级后写入；检查旧归档摘要和可读性，将全套冷备恢复到**另一套新建空数据/日志卷**，不覆盖失败卷。使用原 image ID、原配置、原凭据及设备/网络参数启动，仅替换为恢复卷；核对账号、双盘摘要、MCP/DAV/桌面和再次重启。只有归档能列出文件不算恢复成功。回滚恢复备份时点，升级后写入需要单独处理；凭据撤销和删除事实也须复核，避免旧 Token 重新有效。

### 固定镜像与旧状态

新桌面空间固定当时lock中的OCI digest；以后Host换默认preset版本不会重写已有记录。worker由匹配helper原子分发，依赖合同仍为workerContract1；更换Playwright/Chromium或contract必须重新资格验证，不能仅凭“可启动”宣称兼容所有旧根盘。保留现有空间所需OCI缓存，不按新lock批量清理旧镜像。

发现问题先停止新服务，保留现场和新数据备份。不支持 schema3 的旧二进制不能读取新状态，不能只换回旧镜像继续使用新state；恢复维护前的**整套冷备份**及旧配置/离线包。`state.schema2.backup.json`是审计与恢复输入，不独自代表一致磁盘回滚。升级后新建空间或文件变化需要另外保留，恢复备份也须核对凭据撤销/用户删除事实，避免旧Token重新生效。

## 完成检查

| 检查 | 通过标准 |
|---|---|
| 迁移 | dry-run无写入；锁冲突失败；精确备份；所有旧身份及资源保持 |
| 原功能 | 原有工具目录与能力、MCP凭据、DAV、发布、文件校验及VM回收保持 |
| 新能力 | 桌面空间九工具、自定义空间五工具；按固定镜像复核，Host重启能力不漂移 |
| 桌面 | 真实首帧/键鼠/剪贴板；人工互斥；休眠/重启不增加空白标签 |
| 任务 | 硬期限、取消后代、结果不重放、失败副作用提示 |
| 文件 | 大文件流、Range、原子提交、无覆盖、撤销/中断行为 |
| 回滚 | 旧离线包与整套冷备份可恢复；schema3未交给旧二进制 |

本仓库资格记录与目标部署验收分开。完成测试不代表已更新当前运行服务；实际部署仍需维护窗口。

## SQLite 故障处置

本节仅适用于 Server。先记录实际服务/VM 程序身份、首次错误及停止时间、OOM/重启日志、DB/WAL/SHM 和现有账号/磁盘清单，不将 `malformed` 当作账号丢失。健康检查会返回 503；unhealthy 本身不会触发 Docker 自动重启。故障机制与固定构建见[运行时维护](runtime.md#sqlite-防御)。

1. 暂停客户端写入，确认数据目录没有第二个容器或宿主写入者。保存私有诊断材料，正常停机后完整冷备数据、日志、配置和凭据来源；包括 state、DB/WAL/SHM、根盘与登记卷。自定义 StateStore/SDK 外部目录和嵌套挂载必须逐项覆盖，不能只备父卷。
2. 验证归档可读及 SHA-256，保留双盘、账号及配置基线。复制完整数据库目录到可丢弃的诊断副本，仅在副本移除 SHM、保留主库和 WAL，再用匹配的 SQLite 3.51.3 `agent-space-sqlite-check` 只读检查完整性、外键和运行记录。不能用 `immutable=1` 忽略 WAL；不创建缺失数据库。
3. 完整性或外键失败、未知/非终态 run、强杀/OOM 等均停止自动切换并独立调查。原卷、WAL 和冷备不变；优先恢复最近验证过的完整冷备到独立新卷。数据抢救只操作副本，不自动删 WAL、VACUUM、`.recover` 或建空库导回生产。
4. 副本通过也不等于历史写入全部保留。使用已资格验证的匹配服务和 CLI 做受控容器替换，重新建立进程连接；不热换池、不重建空间或重放结果未知的命令。先在独立卷演练，实际切换按上文升级与回滚流程。
5. 核对原凭据、账号/磁盘摘要、缓存 CLI 与实际 VM 可执行文件，验证 MCP、DAV PROPFIND/GET/Range/写入、桌面及重启。保留失败卷和修复后写入；清理旧容器时不用 `-v`，不得清理仍被使用的业务卷。

### 历史 SQLite-only 修复记录

仅替换 0.7.2 CLI 的旧维护支线已撤销。当前 SDK/runtime 生命周期修补是配对资产，升级统一采用上文的完整镜像、隔离演练和整套冷备回滚流程，避免将新 CLI 单独装入旧服务。历史修复及恢复结果保留在[证据目录](evidence/sqlite-20260930/README.md)，只说明当时输入，不作为当前部署工具或资格。
