# 开发、测试与发布

本文是源码开发与发布资格的维护入口。部署操作见 [Docker 部署](deployment.md)，产品边界与文档入口见[仓库首页](../README.md)。命令在 Linux/WSL 的 `agent-space/` 中运行，使用仓库锁文件。

## 代码职责

| 路径 | 职责 |
|---|---|
| `src/`（agent-engine） | 空间所有者、执行队列、状态、资源观测、任务与控制权；`media.rs` 统一有界图片处理 |
| `src/engine/msb.rs` / `process.rs` | Server 的 VM 适配 / Standalone 的唯一现有空间适配 |
| `agent-space-mcp/` | 两个入口、共享 MCP、认证、管理、桌面和文件/发布代理 |
| `desktop-helper/`、`guest/desktop/` | 受管桌面、浏览器 worker、期限和后代清理 |
| `static-http/`、`webdav/`、`guest/`、`build-support/` | 文件服务、原子文件操作和共同进程边界 |
| `desktop-ui/` | 共用工作台；相对路径适配代理前缀 |
| `docker/`、`images/desktop/` | 两套构建/Compose、Workspace 来源、验证器与离线包 |

共享能力直接复用现有所有者，目标差异留在装配与运行适配。Standalone 不引入 VM、用户登记或第二套任务服务。新增/修改 helper、worker 或 Agent 说明前核对[受管进程规范](architecture.md#受管进程)。SDK/CLI 升级或补丁变更先核对 [MSB 补丁撤销条件](runtime.md)。

执行身份遵循[权限合同](architecture.md#执行身份与环境管理)：Standalone 的 Agent 任务使用 UID/GID1001，系统依赖由镜像提供；Server 的工具、Chromium 和桌面统一使用 VM 内 root。新增文件访问复用同一空间身份，并验证 bash 创建的私有目录在文件工具、browser_run、截图保存和原生上传间可互通，下载同时核对自动化与普通桌面路径；保留 Standalone 对 root 控制文件与进程的保护。

## 源码检查

```bash
cargo fmt --check
cargo build -p agent-space-static-http -p agent-space-webdav -p agent-space-desktop --locked
cargo test --workspace --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build -p agent-space-mcp --no-default-features --bin runcove-standalone --locked
cargo clippy -p agent-space-mcp --no-default-features --bin runcove-standalone --locked -- -D warnings
cargo tree -p agent-space-mcp --no-default-features --edges normal,build --locked
python3 tests/test_managed_cleanup.py
python3 tests/test_file_read.py
python3 tests/test_exec_owner.py
python3 tests/test_msb_patches.py
python3 tests/test_workspace_transfer.py
python3 docker/test_verify_protocol.py
python3 docker/test_verify_dav_large.py
python3 docker/test_package.py
```

先构建当前三个 helper，再串行运行真实 VM 测试；只使用独立资格 Host、容器和新建卷，不指向日常或生产服务。Standalone 的 normal/build 依赖图应排除 microsandbox/libkrun。工作区导入的所有者与权限测试需要隔离 root 环境；普通用户运行的跳过项不能记为通过。

仅修改工具时，可在独立容器运行 `python3 docker/verify-standalone.py --image <base-image> --development-assets --tools-only`，绑定当前 debug 二进制和 UI，执行浏览器及媒体套件并验证容器基础行为。Server 使用 `python3 docker/verify-desktop.py --image <server-base-image> --development-assets --tools-only`，在新建独立卷的真实 VM 中运行同一套件，无需长流程生命周期报告。先构建对应 MCP、Standalone 和 helper；该模式包含个人资料的真实300秒休眠恢复，以及降低额度后经选择器恢复旧页面；报告 full=false，跳过文件流、独立生命周期和真实 UI 套件，不满足完整镜像发布资格。去掉 `--development-assets` 可对最终镜像执行同一范围的检查。

资源紧张时使用 `CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`，不要并行运行完整构建或 VM 生命周期套件。UI 在 `desktop-ui/` 使用 `npm ci`、`npm run build` 和 Playwright；运行真实 UI 套件前准备匹配浏览器、系统运行库和字体。源码检查不替代真实 HTTP、文件、桌面和迁移验证。

## 按变更选择回归

| 变更范围 | 必须核对的行为 |
|---|---|
| 配置/状态 | 合法最小配置、未知/跨目标字段、旧新冲突、默认值与转换不漂移、坏状态/锁/提交故障、原凭据和资源身份 |
| 执行/清理 | 输出洪泛、非 UTF-8、超时部分输出、取消/丢失等待者、提前 EOF、owner 与前台后代收敛，正常后台服务保留 |
| 桌面/控制 | DOM/AX 独立、显式输出、1..8帧与间隔、可选原始PNG保存/命名/路径/真实下载、bash创建私有目录后脚本读写及截图保存权限一致、Page配置默认/覆盖、并发新增与弹窗/新窗口上限、关闭确认后释放、取消保留页面、浏览器关闭后从显示控制重开、个人资料选择器/跨资料上限/最后选择及全部页面休眠恢复、CDP失去响应时仍能闲置回收、页面绑定/关闭、CDP卡死时桌面截图、原生窗口、硬期限、部分输出、不重放、交接取消、清理失败 BLOCKED |
| 工作台/代理 | 真实 RFB 键鼠和中英文剪贴板、双观看者单控制、320/390/宽屏、HTTPS/改写 Host/嵌套前缀、Origin/Cookie/nonce、重启登录失效 |
| 文件/发布 | 文本兼容、read 默认关闭图片、四格式/方向/透明度/首帧、错误扩展名、损坏/超限、超过50 KiB完整图片、12 MiB工具结果、并发/取消/回收；原子提交、无覆盖、权限、ENOSPC 保留原文件、1 GiB 文件流、4 GiB DAV/Range/截短/取消、发布认证与撤销 |
| Server | 凭据/空间隔离、轮换/禁用/删除/同名重建、VM 容量/暂停/异常退出恢复、原盘保留及 MSB 补丁回归 |
| Standalone | 无 KVM、秘密与 UID1001 边界、用户路径不可让 root 代读写、单空间路由、容器限额/信号/重建和卷保留 |
| 迁移/交付 | 精确旧镜像升级和独立卷冷备回滚、选定工作区导入及归档后路径预检、标签并发变化时仍验证同一镜像 ID、归档/OCI/manifest/报告对应实际镜像、离线加载与 Compose |

共同核心变更同时覆盖两目标适用场景；只改文档或目标化编排不要求重跑无关的大文件/VM 套件。每项区分 PASS、FAIL、NOT_EXECUTED；清理失败使对应资格失败。

## 镜像资格与离线包

Server 使用 `bash docker/build.sh`，Standalone 使用 `bash docker/build-standalone.sh`。两者复用 Workspace 依赖来源，MSB 只进入 Server。不要手工改生成的 Workspace lock；构建产物记录 Git 基线及实际输入摘要，未提交构建不能只凭 revision 识别源码。

发布前在独立资源上执行最终镜像资格：

```bash
python3 docker/verify.py --image "$SERVER_IMAGE" --idle-seconds 310
python3 docker/verify-desktop.py --image "$SERVER_IMAGE" --lifecycle "$SERVER_LIFECYCLE_REPORT"
python3 docker/verify-production-upgrade.py --image "$SERVER_IMAGE" --old-image "$EXACT_OLD_IMAGE_ID"
python3 docker/verify-dav-large.py --image "$SERVER_IMAGE" --full-read --stress --evidence target/server-files.json
python3 docker/verify-storage-failure.py --image "$SERVER_IMAGE" --evidence target/server-storage.json
python3 docker/verify-standalone.py --image "$STANDALONE_IMAGE" --full
python3 docker/verify-storage-failure.py --image "$STANDALONE_IMAGE" --evidence target/standalone-storage.json
python3 docker/verify-workspace-import.py --image "$STANDALONE_IMAGE" --old-image "$EXACT_OLD_IMAGE_ID"
```

将生命周期脚本输出的 `result.json` 路径赋给 `SERVER_LIFECYCLE_REPORT`。资格脚本在开始时解析镜像 ID，后续初始化、启动、备份/回滚都使用该 ID；输入标签在运行期间改变不能改变实际被测镜像。打包器也按报告的 `image_id` 读取镜像，原标签仅作来源说明。Standalone 的 4 GiB 文件验证只指向本仓库新建且带资格标记的容器；其运行方式和客户端范围见 [DAV 参考](webdav.md)。`--development-assets` 绑定当前二进制用于开发回归，报告明确为开发资产，不能通过正式打包门槛。

打包按目标核对共同完整套件、4 GiB 文件、20 次真实写满与固定拒绝上传截止、成功清理、精确镜像和当前源码摘要；Server 另需旧生产版本升级/完整冷备回滚报告。命令和加载步骤见[部署指南](deployment.md#3-验证与离线包)。导出归档、内嵌 release 元数据、manifest 和报告必须一致；使用包内 Compose 实际初始化和启动，并检查未遗留本次资格资源。

固定结果登记到[验证记录](validation-records.md)，证据保留确切源码、镜像和执行范围。新源码检查不能改写旧镜像的通过记录；重建后按影响重跑。生产替换、registry 推送、外部平台现场联调和目标 NAS/ARM 验收分别记录，不由资格脚本自动执行。

## 工作目录维护

源码、锁文件、当前参考和仍被引用的脱敏固定证据入 Git；`target/`、`docker/.build/`、UI 构建/测试输出、依赖缓存及发布归档是本机产物。已被后续验收覆盖、且现行文档与工具均不再引用的中间演练记录不随源码保留。公开 Git 历史从整理后的首次提交开始，不含此前的私有开发记录；旧报告中的 Git SHA 仅作为当时输入的标识，详见[验证记录](validation-records.md)。

`target/` 也可能存有恢复快照和离线包，不能整目录按缓存删除。只清理来源明确、已结束且不被进程或容器使用的临时产物；隔离资格归档须先核对成功和资源清理记录。保留实际业务备份、正式离线包、未定位故障的诊断材料，以及 `docker/configs/`、`docs/local/` 等本机运维配置。不要用 `git clean -fdx` 代替分类核对。

## 文档维护

`README.md` 是产品和阅读入口；`docs/` 直接保留少量长期中文手册，按部署、操作、接口和实现职责组织，使用稳定的英文小写连字符文件名。不另建重复目录页，不将每个功能、审查或发布拆成一份文档。

- 行为和参数由对应参考文档维护；架构维护所有权与生命周期；部署写首次安装，运维写现有数据的升级/恢复，开发指南维护检查与交付流程。其他文档链接到权威段落，避免复制整套参数、命令和限制。
- 已实现的方案、阶段清单、被替代的调研结论不作为长期文档保留。仍有效的设计约束并入架构或参考，操作经验并入运维，未解决的 MSB 问题集中在运行时维护。删除前核对源码、调用方和独有内容。
- `docs/validation-records.md` 仅索引固定输入、结果及未验证边界；`docs/evidence/<topic>-<date>/` 保存被索引的必要证据，不按文档语言复制。保留来源身份与日期，旧结果不改写成新版本通过。无引用且被后续验收替代的中间记录可删除；仍用于补丁、迁移或未定位故障的证据继续保留。
- 修改路径时同步 README、AGENTS、相互链接、锚点和离线包生成器。提交前检查本地链接/锚点、示例路径及 `git diff --check`；涉及交付入口时运行 `python3 docker/test_package.py`，在临时目录调用 `copy_manuals` 核对离线文档和引用是否齐全。纯文档变更不重跑无关 VM 套件。
- 发布前扫描 Git 跟踪文件中的真实 Token/私钥、凭据摘要、生产域名/IP、本机路径、账号/空间标识、业务文件清单及截图。私人部署记录留在受控运维存储，不随仓库或离线包发布；Git ignore 与 Docker ignore 分别排除本机配置和密钥，不能将未入 Git 等同于不会进入构建上下文；只保留必要的脱敏验证结果和产物身份，标明投影及原始日志摘要的范围。
- 不把本机地址、私有备份路径或个人实验数据写成通用安装要求。配置模板仍位于 `agent-space/docker/configs.example/`，运行时给 Agent 的说明仍由 `agent-space/guest/README.md` 维护；不为双语规划修改这些运行资产。

### README 媒体维护

首页媒体集中在 `docs/assets/`：

- [首屏插图](assets/runcove-hero.svg)与[七产品协作图](assets/measix-ecosystem.svg)是可编辑、自包含的 SVG，沿用 Measix 的产品标识与配色；插图是概念表达，协作图是产品职责与演进方向，不作为运行截图或全部集成已交付的证明。
- [桌面截图](assets/workspace-desktop.jpg)、[产物面板](assets/workspace-files.jpg)和[操作关键帧动图](assets/workspace-walkthrough.gif)拍摄于 2026-10-16 的独立 Standalone 演示容器。使用已有验证镜像 `sha256:b13c172f4e8a21c42d256dbbea21ecb6fcd4a578847851c85e98fd47a25c2993` 作为后端基底，前端使用本次源码构建并统一“工舱”名称。它们不是当前源码的新发布资格记录。
- 演示资料为虚构的巡检 CSV、复核清单与网页报告，通过 MCP 写入 `/workspace/inspection-weekly` 并打开浏览器，交付副本放入 `/workspace/agent-space/artifacts`。画面中的报告是工作区文件，外层工作台、文件面板和控制状态来自真实产品；动图是观看、接管、文件面板、交还控制的实机关键帧，不是完整模型执行录像。
- 更新时使用独立容器和新建数据卷，仅放入示例资料；以当前前端构建捕获实际页面，核对所有文字、文件名、地址和图片元数据。凭据不进入 URL、截图或入库文件。检查桌面与窄屏布局，并确认离线包带齐媒体；完成后清理本次演示容器和卷。

### 双语结构规划

英文结构目前仅为规划，中文仍是当前维护版本，不创建英文译文、空目录、占位页或不可用的语言切换链接。后续有实际译文时：

| 入口或内容 | 中文 | 英文（后续） |
|---|---|---|
| 仓库首页 | `README.md` | `README.en.md` |
| 长期手册 | `docs/<name>.md` | `docs/en/<name>.md`，沿用同一文件名和章节职责 |
| 固定验证索引 | 当前 `docs/validation-records.md` | 翻译索引链接同一份 `docs/evidence/`，不复制或翻译证据文件 |
| Agent 约束、配置、来源和运行时资产 | 保持现有路径 | 不增加语言副本 |

按完整页面逐篇引入；仅对已存在的对应页增加互链。中文页面继续使用相对链接，英文页面按自己的目录校验到配置、源码和共同证据的相对路径；缺少译文的条目直接指向中文并标注语言。行为变更先更新权威中文内容，同次变更同步已有译文；无法同步时明确标记范围及对应源码版本，不将旧译文作为当前合同。新增翻译目录时再调整离线包复制和链接检查，不提前建设文档站或翻译框架。
