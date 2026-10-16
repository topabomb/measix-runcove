# Docker 构建配置

本目录包含 `agent-space` Docker 镜像构建时使用的配置文件。
请将实际配置放在 `docker/configs/`（已 .gitignore 排除），如需自定义可从此目录复制后修改。

## 配置说明

### config.toml

这是 agent-space-mcp 的主配置文件（TOML 格式）。详见：

- 首次初始化与启动：[Docker 部署](../../../docs/deployment.md#2-配置与首次启动)
- 用户管理与 MCP 接入：[功能与接入参考](../../../docs/server-api.md)

### sing-box.json.template

sing-box 透明代理配置模板（JSON 格式）。sing-box 为**运行时可选**功能，
通过 volume mount 注入配置即可启用：

```bash
-v ./你的配置文件.json:/etc/agent-space/sing-box/config.json:ro
```

此处仅保留文件名占位，无实际内容。请参考 sing-box 官方文档编写配置：

- 官方 GitHub：https://github.com/SagerNet/sing-box
- 官方文档：https://sing-box.sagernet.org/configuration/
- geoip 规则集：https://github.com/SagerNet/sing-geoip
- geosite 规则集：https://github.com/SagerNet/sing-geosite
