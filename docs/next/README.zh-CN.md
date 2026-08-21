# GoWild 未发布文档

[English](README.md)

本目录是尚未作为稳定版本发布的 GoWild 行为的文档来源。

## 产品模型

GoWild 将三个选择彼此分离：

1. **编程 CLI** — 首先支持 Codex CLI 和 Claude Code。
2. **网关** — MindsHub Inference 或自定义协议兼容端点。
3. **模型** — 所选网关以该 CLI 可兼容方式公开的任意模型。

GoWild 在持久化、由服务器拥有的终端中启动用户实际安装的 CLI。它为每次启动提供独立路由，而不修改用户常规的 Codex 或 Claude 配置。路由在不包含凭据的情况下保存，并在恢复智能体时重新应用。

## 当前指南

- [从源代码安装并验证（英文）](INSTALL.md)
- [网关配置与 CLI 路由（英文）](gateways.md)
- [Socket API 模式](api/gowild-api.schema.json)
- [未发布变更](CHANGELOG.md)

首次运行及 **设置 → 网关** 中均可进行网关配置。当前支持 MindsHub 预设、自定义网关、安全凭据替换、协议测试、模型发现、每个 CLI 的默认模型，以及受管理的启动和恢复。

## 发布状态

GoWild 尚无公开二进制版本、托管安装程序、网站或更新通道。目前仅支持本目录中的源代码安装方式。在 GoWild 自有制品、签名、清单和干净安装流程通过审查之前，所有继承的发布路径都会保持禁用。

## 历史文档

嵌套的 `website/` 树以及同级的 `docs/preview` 和 `docs/versions` 树是冻结的源代码导入记录。它们描述的是被导入的产品，而不是 GoWild，绝不能作为 GoWild 文档构建或发布。请参阅 [`docs/README.md`](../README.md) 和 [`PROVENANCE.md`](../../PROVENANCE.md)。

所有 GoWild 工作仅属于 [`ianu82/gowild`](https://github.com/ianu82/gowild)。不得向源代码出处项目发送代码、issue、PR、请求或同步自动化。
