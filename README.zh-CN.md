# GoWild

[English](README.md) · 简体中文

GoWild 是面向编程智能体的持久化终端运行时。它将编程 CLI、LLM 网关和模型作为三个相互独立的选择。

GoWild 从 [`PROVENANCE.md`](PROVENANCE.md) 中记录的 Apache-2.0 源代码快照起步，并正在发展为采用 MindsHub Cowork 品牌的独立产品。目标是让已安装的 Codex CLI 和 Claude Code 通过兼容协议连接 MindsHub Inference 或自定义网关。

## 产品方向

- 持久化工作区、标签页、窗格、会话和远程重连。
- 检测智能体的工作、阻塞和空闲状态。
- 原生支持 Codex CLI 和 Claude Code。
- Codex 使用兼容 OpenAI Responses API 的网关。
- Claude Code 使用兼容 Anthropic Messages API 的网关。
- 首个预设为 MindsHub Inference；自定义网关使用同一适配器架构。
- 在 TUI 中安全管理凭据，并为每个 CLI 选择模型。

网关配置仍在开发中。本仓库目前不发布稳定二进制文件、安装程序或更新通道。

## 仓库边界

GoWild 的所有工作仅在 [`ianu82/gowild`](https://github.com/ianu82/gowild) 中进行。不得向 Herdr 项目提交 GoWild 代码、issue、PR 或支持请求。只读源代码归属及精确导入基线见 [`PROVENANCE.md`](PROVENANCE.md)。

## 许可证

Apache License 2.0，详见 [`LICENSE`](LICENSE)。
