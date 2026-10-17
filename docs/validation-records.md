# 固定版本验证记录

本页保留源码检查、镜像资格和实际部署的证据索引，三者不互相替代。每份报告只适用于自己的输入和运行范围；本页不充当当前服务状态探针。接口与行为以对应参考文档为准，验证方法见[开发指南](development.md)。

公开报告可能是去除现场信息后的投影；源码和镜像摘要保留原值。标为原始输出的日志摘要不等于脱敏文件的当前摘要，私人部署快照不随仓库或离线包发布。

公开 Git 历史从整理后的首次提交开始，未保留此前的私有开发历史。下列旧报告及其证据中的 Git SHA 是当时输入的标识，不能在公开仓库中检出，也不能据此复现旧构建；源码摘要和镜像 ID 继续标识原始验证对象。首次提交包含之后的修复和文档整理，不能直接沿用旧镜像的资格结论；发布前仍须按当前源码重新构建并验证。

## 浏览器与图片工具调整（2026-10-17）

基线 `61dbf67` 加本轮实现，最终构建输入 SHA-256 为 `0c48b4974e55c4ae8a87a9626dbd95aeea27bcee6c5a618ce344428f1acb33b4`。两个入口继续共用工具、任务和文件执行链路。`browser_observe` 分开 DOM/AX，`browser_run` 只返回显式输出，`browser_task` 同时管理截图序列；新增 `screen_observe`，默认单张、最多8张，可选间隔及原始 PNG 保存目录，返回各帧和已保存路径。`read` 增加默认关闭的 multimodal 图片读取，Rust 统一有界处理；工具结果预算为12 MiB，默认文本输出上限保持不变。合同见[浏览器与图片工具](browser-desktop.md#浏览器与截图工具)。

完整 Rust workspace 串行 **315 passed / 0 failed / 0 ignored**，最终受影响模块另通过 MCP 27 项、desktop helper 6 项；两种构建的 Clippy、Server/无 MSB Standalone 构建、无 MSB 依赖检查和 UI 构建通过。Python 清理8项、执行所有权5项、MSB补丁4项、协议8项、DAV验证器3项、打包5项通过；独立 root 容器中的工作区导入9项通过。检查和二进制摘要见[验证摘要](evidence/runcove-tools-20261017/verification.json)。

按用户要求收缩为快速验收，只使用一个新建容器及卷，绑定当前开发二进制：真实 MCP 浏览器11项、媒体8项、文件流3项通过，另通过短流程启动、权限、退出和重启检查。媒体覆盖四格式、方向和透明度、GIF/APNG/WebP首帧、接近10 MiB源文件、超过50 KiB的完整图片、多 MiB工具结果、1..8帧和间隔、页面关闭、Chromium卡住时的桌面观察、原图紧凑命名及路径、认证HTTP下载、保存权限失败、并发和取消保留输出。

首轮长流程在上述三个套件通过后按用户要求停止，300秒休眠及真实UI套件未完成，未启动第二个部署验收；隔离容器与卷已清理。该轮仅做源码及开发资产验证，没有重建发布镜像、部署生产、推送镜像或完成外部Agent/模型/网关的多模态联调，不能作为最终镜像资格报告。

随后对全部九个工具的参数、说明、返回、预算及执行链路复审，修正 BM25/II* 普通文本误判、很小文本限额截断文件协议及图片错误、旧截图视图忽略显式任务历史；补齐有界短读循环、参数提前校验、保存错误中的目录信息，并移除重复错误码解析。复审源码输入 SHA-256 为 `16d3e95d65205e0389883f53ed603e7cd470da52d1b940c83a468ba76366851a`。8字节输出限额的真实 VM/MCP 回归、文件分类与短读3项、MCP单元/合同32项、helper 6项、清理8项、打包5项及两种构建的 Clippy 通过。新增 `--tools-only` 快速验收模式，单个隔离开发资产容器的浏览器11项、媒体8项及基础重启检查通过并清理；报告 full=false，不绕过发布资格。详细检查摘要追加在同一[验证摘要](evidence/runcove-tools-20261017/verification.json)的 review 字段。

当时对实际 Server 调用中出现的自定义截图目录权限失败仅补充了参数说明，构建输入为 `83fe72521872f94f191aa99b2c10a6131491ca59769a18b832466e1b2f4f873b`，5项工具合同回归通过，发布二进制重新构建并验证实际链接 SQLite 3.51.3。后续权限复审确认这是工具执行身份不一致，已按下述规则修正，不能将当时的限制说明作为当前合同。

复审及说明修正分别提交为 `abda7df`、`b083dc4` 后，按用户要求直接更新现有生产 Server 容器，未创建备份或执行迁移。只原子替换 MCP、desktop helper 及对应 release 元数据并重启；服务二进制 SHA-256 为 `623dc2a6198fcc3e48ad4e7e74f7ed5be6a4ea3231349a8672741106d0300196`，desktop helper 为 `75f3d3c182a6a579f53e0e442f0d68ed515bc96f197260385ab9d43420854292`。原镜像 ID、配置、挂载、运行时和原有两个账号记录保持不变，其他服务未重启；容器健康，本次启动运行时错误为0。用临时隔离账号实际调用九个工具，核对超过50 KiB完整图片、DOM/AX、显式脚本结果及任务查询、连续截图、原图文件路径和认证下载哈希；账号及其资源由正常删除流程清理，上传二进制临时文件已移除。结果见验证摘要的 production_binary_update 字段。

这仍是原镜像上的容器可写层覆盖，没有重建镜像或离线包；由原镜像重新创建容器不会包含本次程序更新，需重新应用二进制。现场调用验证不延伸为完整镜像资格或外部Agent/模型/网关的多模态联调结论。

随后以 `47b8d58` 为基线复审全部九个工具的权限链路，修正截图保存、`browser_run` 用户脚本和产物枚举固定使用桌面身份的问题。它们现在与空间命令和文件工具一致：Server 使用 VM 内 root，Standalone 保持 UID1001；任务目录及结果归属检查同步匹配。Chromium、桌面及内部观察进程继续使用 UID1001，无新增配置、权限参数或目录白名单。构建输入 SHA-256 为 `4a2fa14a42ed8441bb75c4367d474de74c209428103943a2a69fdbec0d75eaca`，工具说明、工作区指引和合同文档同步更新。

权限复审的快速验证通过 helper 6项、MCP 32项、清理8项、执行所有权5项、打包5项、全目标及无 MSB Standalone Clippy。隔离进程的8项检查复现旧版拒绝并验证 Server 私有目录保存、产物枚举、脚本身份及私有文件上传，Standalone 仍拒绝 root 私有路径。只启动一个 Standalone 验收部署，浏览器11项、媒体9项、秘密隔离和基础重启通过，full=false，容器与卷已清理；新增共同回归覆盖 bash 创建0700目录后的文件读写、浏览器脚本、页面/桌面原图、认证下载和产物列表。发布程序重新构建，SQLite 实际链接版本为3.51.3。首轮开发资产重启受测试中重编译绑定文件干扰，固定输入后复验通过。详情见验证摘要的 workspace_permissions_fix 字段。

## 工具描述与生产二进制更新（2026-10-16）

提交 `7e3f206` 统一八个 MCP 工具的远程工作区归属说明：`bash`、`browser_observe` 保留工具描述，`read`、`write`、`edit` 的 `path`、`browser_run` 的 `code` 和 `browser_task` 的 `task_id` 补充归属，`web_publish` 在工具描述首句补充。每个工具的描述与参数描述合计出现一次 `Runcove's remote workspace`；未改执行逻辑或参数结构。`cargo fmt --check` 及无 MSB 构建下的 `test_web_publish_mcp` 五项测试通过。

以锁定依赖和 SQLite 3.51.3 静态库构建 release MCP 程序，在原生产容器内原子替换二进制并执行重启，沿用原运行时、helper、UI、配置和数据卷。构建输入 SHA-256 为 `3c7f88090ecc2b981be67270927f0997da60a8777302ea13b504fb4485075fc6`，部署二进制 SHA-256 为 `aa9db24889feb603e5c191216084400b6e1de7e9dae303bbc1ad442faaff8107`；容器内 `release.json` 的程序摘要与覆盖来源已同步更新。

本次是原镜像 `sha256:413c3411b962e140b89f9f1a7195b4350ff92af2f76f3dec5a9fe3a00c129260` 上的容器可写层覆盖，未重建镜像或离线包；重新创建容器不会自动包含本次覆盖。未备份、未执行生产功能验收，既有镜像资格不延伸至本次二进制更新。构建与部署临时文件已清理。

## 上游介绍与资格脚本审查（2026-10-16）

基线 `459c385` 加本次变更：在[仓库首页](../README.md#microsandbox-与项目定位)补充 microsandbox 介绍、致谢及三个固定上游版本的定位对照；比较来自官方/项目自身文档，没有执行跨项目运行或性能测试。文档数量保持不变。

审查现有生产验收改动，确认 DAV 自建临时容器需同时清理匿名日志卷；修复桌面验收脚本结束后未恢复调用方管理员环境变量的问题。无 Docker 的回归覆盖失败诊断中的三类测试凭据脱敏、环境恢复、清理失败记录及匿名卷回收：`docker/test_package.py` 5 项、`docker/test_verify_dav_large.py` 3 项通过。当时另核对生产材料的镜像/来源关联及报告引用；后续公开化仅保留必要的脱敏结果投影。20 轮唤醒摘要已与本机原始报告及逐轮结果核对，投影规则写入 `wake-cycles.json`。

本次仅修改文档、验收驱动和回归测试；未改服务/guest 实现，没有重建镜像或重跑真实 VM/桌面资格。下节生产结果属于其记录的既有镜像与执行日期，不是本次回归重新取得的结果。

## 生产 Server 升级（2026-10-16）

历史部署镜像为 **Runcove Server `runcove-fc010ed2726e9bec`**，image ID **`sha256:413c3411b962e140b89f9f1a7195b4350ff92af2f76f3dec5a9fe3a00c129260`**；Git 构建基线 `d5040e2`，实际构建输入 SHA-256 为 `fc010ed2726e9bec25c3872caeb69aba65078db53e8f3518526becca06380811`。这是固定历史结果，不表示当前源码重新构建或当前服务状态。见[公开结果摘要](evidence/runcove-production-20261016/verification.json)。

- 同一最终镜像通过生命周期、完整桌面、精确旧生产版本升级/独立卷冷备回滚、4 GiB DAV 和真实写满资格；构建内 177 项 MSB 回归通过。真实 UI 四项端到端、两段实际五分钟休眠/恢复、1 GiB 文件、4 GiB 逐字节读取、80 次取消、20 次写满通过。见[资格](evidence/runcove-production-20261016/qualification.json)、[UI 日志](evidence/runcove-production-20261016/server-ui.log)和[升级回滚](evidence/runcove-production-20261016/upgrade-rollback.json)。
- 现场升级记录核对过原账号、凭据、固定镜像及工作盘保留，后续生产功能与重启验证完成。公开树仅保留验证结论和发布产物身份；主机、域名、账号/空间标识、配置、磁盘清单、备份位置和真实部署截图不公开。
- 首次休眠后观察缺少 `health` 字段，原测试未保留工具原始响应，原因未定。最终完整资格及 20 轮关闭/唤醒通过，不能据此声称首次异常已修复。见[首个失败](evidence/runcove-production-20261016/initial-wake-failure.log)及[唤醒结果](evidence/runcove-production-20261016/wake-cycles.json)。

## 源码审查与维护整理（2026-10-16）

### 双版本与拆分前实现的对照审查

基线是规划提交 `a8f50c53618388115aa671a7072ac4151c58b158` 的父提交 `a8629c1`，审查起点为 `4a88cfc`。按产品边界、共享核心、Server 行为保留、Standalone 适配、认证/路由、配置转换、状态不变量、进程/helper、浏览器/文件、镜像及交付/迁移逐项对照，详细落点与复验见[对照报告](evidence/runcove-review-20261016/comparison.json)。

结论：共享核心和两个运行适配的划分符合产品目标。Server 保留管理 API、原空间身份、凭据与 VM 生命周期；Standalone 复用工具和工作台，固定一个空间，不包含 MSB、管理 API 或持久用户清单。两者还存在隔离方式、资源/生命周期配置和持久化内容的必要差异，不能将区别概括成只去掉管理 API。Workspace 为 VM rootfs，不在其中嵌套 MSB。首版平台仍是 Linux/amd64，NAS/ARM 需分别验收。

本次新增修复：

- 资格脚本检查 ID 后仍以可变标签启动，可能让报告与实际被测镜像不一致；现在初始化、运行和旧版回滚均固定已检查的 ID，打包器也按报告 ID 选择镜像。模拟标签在检查后变化，修复前 5 个场景失败；修复后覆盖 7 个验证入口及打包选择。
- 工作区导入只校验原归档，预览遗漏旧暂存重定位新增的父目录冲突；现在预览与 apply 都在创建 stage 前校验。原实现实际提取会因冲突失败，未复现越界写入。
- Standalone 的 DAV origin 仅含空白时，配置校验允许留空但装配仍尝试解析 URL，导致启动失败；现在与校验使用相同空值语义，并纳入完整 Standalone 资格输入。

最终完整 Rust 复验 **307 passed / 0 failed / 0 ignored**，两种构建的 Clippy、无 MSB 依赖检查、Python 回归和 UI 静态检查通过；独立 root 容器的 9 项导入测试、7 个验证入口及打包选择的标签变化回归通过。真实 Standalone 容器验证空白 DAV origin 修复前退出、修复后监听正常，容器与卷已清理。

初次完整 Rust 回归的 Server DAV VM 测试在连续 HEAD 的第 75 次返回 503，日志显示运行时准入的 `Permission denied`；单独复测及随后完整复验通过。原因尚未定位，首次失败与后续复验分别留存于对照报告，不能从后续成功推断已修复该异常。

该轮审查仅提交源码与证据，未重建发布镜像、生成离线包、修改当时的生产环境或推送 registry；下列旧镜像记录保持原身份和日期。

### 前次审查修复与验证范围

本轮复核共享核心、两个入口/运行适配、认证与代理、状态/资源、进程/helper、浏览器/文件、构建/验证/打包及文档关系，修复：

- 新格式配置转换误用旧监听/容量默认值；转换与正常加载改为同一解析入口。
- 本地执行 owner 提前关闭输出后，进程等待遗漏取消和超时回收；输出 EOF 与进程退出继续由同一 monitor 持有。
- 接管请求被取消后遗留 TRANSITION；释放交接状态，但原操作仍占位，清理失败继续 BLOCKED。
- root 桌面 helper 经用户产物目录链接枚举私有文件元信息；完整遍历下放 UID1001 子进程。
- VM CPU 从 u32 窄化到 u8 可截断，非法配额未在启动前拒绝；配置校验并在创建任何磁盘前检查转换，零内存/盘和输出限额一并报错。

产物目录问题在上一版精确镜像上复现为 HTTP 200 并返回 root 私有文件名；修复后的开发资产容器回归要求拒绝且不泄露元信息，并验证正常产物枚举恢复。前述配置、执行和接管问题也有失败后通过的回归。

源码、运行二进制和测试范围见[本轮审查报告](evidence/runcove-review-20261016/verification.json)。完整 Rust/Clippy、无 MSB 构建、进程/迁移/打包回归与独立容器桌面场景分别记录。已完成计划、旧文档索引及过程性发布流水已移除；有效的产品边界、回归矩阵和操作步骤归入仓库首页、架构、开发与部署/迁移指南。

**该轮审查没有重建发布镜像或重新生成离线包。** 下节 2026-10-15 镜像及离线包不含这些修复，保留为历史资格证据，不能当作当前源码的发布结果；后续发布须使用新源码摘要，重新通过相应镜像门槛。该轮未修改当时的生产环境。

## Runcove Server / Standalone 分离验收（2026-10-15）

同一仓库共享 MCP、基础工具、浏览器、工作台、控制权、文件和发布核心。Server 保持 Docker + MSB VM 多空间与管理 API；Standalone 为一个使用者、一个空间的 Docker 容器，不带 MSB/KVM、管理 API 或静态多用户模式。Workspace 是 Server 使用的空间 rootfs，不在空间中嵌套 MSB。配置按目标收紧职责和默认值，旧配置显式转换并保留有效值；Server 状态仍为 schema3，强化身份/资源不变量，Standalone 不建立持久账号库。

当时应用版本 **`runcove-2e50d622ab9e4573`**，构建输入 SHA-256 **`2e50d622ab9e45736a9d4816831eede4656ba9d8783b2a34c15d8a42c7081532`**；构建时 Git 基线为规划提交 `a8f50c5` 加本轮实现，不能把该 revision 单独当成最终源码身份。

| 目标 | 最终镜像 ID | 资格范围 |
|---|---|---|
| Server | `sha256:e5913326b6f3bac140e4524ffd8468bb0d77aac353c674430308877481e94105` | 生命周期/账号/发布、真实 VM 桌面、1 GiB 文件、4 GiB DAV、磁盘写满、旧生产升级回滚 |
| Standalone | `sha256:b13c172f4e8a21c42d256dbbea21ecb6fcd4a578847851c85e98fd47a25c2993` | 无 KVM 启动、单空间/秘密/UID 隔离、真实桌面、1 GiB 文件、4 GiB DAV、rclone/FUSE、磁盘写满、卷恢复 |

- 源码：当前三个 helper 先构建，完整 workspace 串行 **303 passed / 0 failed / 0 ignored**；全目标 Clippy `-D warnings`、fmt、Standalone 无默认 feature 编译及 normal/build 依赖图通过。依赖图排除 MSB/libkrun；受管清理 6 项、执行所有权 5 项、文件迁移 8 项和协议/DAV/打包回归通过。详见[源码检查](evidence/runcove-split-20261015/code-validation.json)。
- 两目标使用同一组真实浏览器、文件和生命周期套件；各有 **4 项 Playwright 端到端通过**，包括真实 RFB 键鼠/剪贴板、双观看者、人工接管不阻塞基础工具、实际超过 300 秒被动观看休眠/标签恢复，以及 HTTPS + 改写 Host + 嵌套前缀。Standalone 另验证丢失桌面 owner 后 BLOCKED、控制进程死亡后数据子进程收敛、重启后旧登录失效和工作卷保留。报告：[Server 桌面](evidence/runcove-split-20261015/server-desktop.json)、[生命周期](evidence/runcove-split-20261015/server-lifecycle.json)、[Standalone](evidence/runcove-split-20261015/standalone.json)。
- 两目标完整读取 **4,294,967,296 bytes** 并逐字节核对，验证跨 2 GiB/尾部范围、真实截短、旧 ETag、80 次取消和后续可用；各 **20 次真实 8 MiB 文件系统 ENOSPC** 返回 507、保留原文件且可继续写入。拒绝 PUT 后即使每 40 ms 继续发数据，连接仍在同一个约 2 秒截止收敛。报告：[Server 文件](evidence/runcove-split-20261015/server-files.json)/[写满](evidence/runcove-split-20261015/server-storage.json)、[Standalone 文件](evidence/runcove-split-20261015/standalone-files.json)/[写满](evidence/runcove-split-20261015/standalone-storage.json)。Standalone 还通过真实 [rclone/FUSE](evidence/runcove-split-20261015/standalone-rclone.json) 挂载读写；FUSE 权限仅授予独立测试客户端。
- 在新建隔离卷中，用精确规整前生产镜像 `sha256:9ee5cd5fd3998d5defc005b90b1ace906f48484f8fbb680a50a0e6ab7e76ce06` 创建真实 VM，再切换最终 Server，原凭据、空间/盘身份、文件/profile、桌面/DAV/发布和重启恢复通过；完整升级前冷备恢复到另一新卷后旧版回滚通过。相同旧镜像选定工作区导出到最终 Standalone，246 条目、所有者映射、原源未改、标签/profile/链接和用户可写性通过。报告：[升级/回滚](evidence/runcove-split-20261015/production-upgrade.json)、[单空间导入](evidence/runcove-split-20261015/workspace-import.json)。资格容器和卷均完成清理。
- 实际模型经正式 HTTP MCP 完成“观察→CSV→浏览器上传→下载→MCP 摘要→工作台下载”链路，65,536 bytes 摘要一致；真实连接的中英文 320/390/1440 六种布局已查看，无溢出或页面错误。这是较早候选 `37a033…` 上、未变的共同 MCP/UI 组件证据，不能标成最终镜像运行记录。见[模型与布局](evidence/runcove-split-20261015/model-workflow.json)。
- 较早一次完整 UI 运行出现休眠后恢复为新标签页，根因尚未确定。后续最终镜像完整套件均通过，另有 [20 次真实浏览器关闭/重启恢复](evidence/runcove-split-20261015/desktop-restore-cycles.jsonl) 通过；这些结果不等于已经定位或证明修复最初偶发失败。

离线包的加载、Compose 和摘要以[离线包检查](evidence/runcove-split-20261015/packages.json)为准；[总证据](evidence/runcove-split-20261015/verification.json)记录最终身份。子代理只读审查发现的问题已修复，见[实现审查](evidence/runcove-split-20261015/implementation-review.json)。本次资格平台为 Linux/amd64；没有 ARM/目标 NAS 验收、registry 推送或生产切换，升级驱动使用的精确旧镜像见[公开镜像基线](evidence/runcove-split-20261015/production-baseline.json)，账号和拓扑快照不公开。


## 工作区恢复记录（2026-10-14）

历史恢复只导入工作区文件，不导入旧根盘、账号状态或 MSB 数据库，目的空间身份和凭据保留。27,722 个普通文件在离线恢复及重启后逐项校验，源 UID/GID、权限和 66 个符号链接恢复；旧上传暂存归属冲突通过完整留存旧目录、由正常 DAV 创建新归属处理。DAV PROPFIND、抽样 GET/摘要和实际文件入口通过，当时没有完成目的账号的公网桌面登录/控制验收。

公开材料只保留[恢复结果投影](evidence/fresh-production-20261014/restore.json)和[独立镜像资格](evidence/fresh-production-20261014/qualification.json)，不包含业务文件清单、原始账号、部署配置或备份位置。具体维护步骤见[运维手册](operations.md#将完整工作区备份恢复到新空间)。

## MSB 运行时固定证据

| 固定范围 | 保留的结论与证据 |
|---|---|
| 2026-10-13 生命周期补丁基底 | 同源 SDK/CLI、SQLite 3.51.3；177 项构建内回归。见[构建验收](evidence/msb-lifecycle-20261013/verification.json)与[配方](evidence/msb-lifecycle-20261013/Dockerfile.sdk-runtime-cache) |
| 2026-10-13 应用与真实重启 | 293 项串行 Rust、12 项隔离恢复、原盘/凭据保留。见[复核](evidence/msb-lifecycle-review-20261013/verification.json)、[恢复资格](evidence/msb-lifecycle-review-20261013/qualification.json) |
| PID 复用原始失败 | 旧版 crashed + 无关 PID 复用时首次 DAV 超时；原现场最初崩溃原因仍未知。见[失败基线](evidence/dav-restart-20261013/baseline-pid-reuse.json) |
| 2026-10-11 部分帧取消 | 确定性 partial-frame 回归与当时实际范围见[记录](evidence/dav-frame-read-20261011/verification.json) |
| SQLite 锁/WAL 机制与旧版恢复 | 机制见[运行时维护](runtime.md#sqlite-防御)，操作见[故障处置](operations.md#sqlite-故障处置)，固定测试输入见[证据目录](evidence/sqlite-20260930/README.md) |

补丁源码、上游链接、历史 relay 复现和逐项撤销条件统一由 [运行时维护](runtime.md)保存。本页不重复维护上游“当前状态”；未被索引的过程性开发/部署流水不随公开源码保留。

## 持续有效的验证边界

- HTTP 健康、VM Running 或单个 MCP 请求不能证明桌面、DAV、外部平台或恢复数据已验收。
- 工作区转移与整套服务回滚是不同操作；只有全套一致冷备能恢复服务状态和磁盘。
- DAV 锁不约束 MCP/bash，目录递归可部分完成，不能承诺 Office 协作或所有网盘客户端兼容。
- 真实最终镜像、开发资产绑定测试、模型操作链路和目标部署现场分别标识；一次成功也不证明所有传输竞态或长期可靠性。
- Linux/amd64 的 Docker 资格不等同于 ARM、目标 NAS、生产切换或 registry 发布。
