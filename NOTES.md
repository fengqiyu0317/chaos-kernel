# Chaos 项目记录迁移说明

更新日期：2026-06-19

## 工作目录约定

以后在本项目中使用 Codex 和 GitHub 时，默认进入：

```bash
cd "/mnt/d/Tomato_Fish/豫文化课/新时代/大二春/操作系统/chaos"
```

然后再运行：

```bash
codex
git status
```

这样 Codex 会优先读取 `chaos/AGENTS.md`，Git 命令也会作用于 `chaos/.git` 对应的独立仓库。

## GitHub 仓库状态

`chaos/` 已经是独立 Git 仓库，不需要迁移 `.git`：

```text
origin   https://github.com/fengqiyu0317/chaos-kernel.git
upstream https://github.com/peterzheng98/chaos.git
```

迁移原则：

- 保留 `chaos/.git` 原样不动。
- 后续提交、推送都在 `chaos/` 内执行。
- 外层“操作系统”仓库只作为课程资料总目录，不再承载 Chaos 的日常提交。

## 已迁移记录

- `../AGENTS.md` -> `chaos/AGENTS.md`
- 新增 `chaos/TASK.md`
- 新增 `chaos/NOTES.md`

## Codex 工作要求

- 长任务或上下文即将不足时，先更新 `TASK.md` 或 `NOTES.md`。
- 交接摘要至少记录目标、已完成修改、关键文件、测试结果、未解决问题、禁止修改位置。
- 代码改动需要区分 `// HUMAN` 和 `// AGENT`。
- 修改 `kernel-sim` 时不要触碰 `kernel/src/kernel.rs`。

## 当前状态

本次迁移只涉及文档和记录文件，未修改内核实现，未运行测试。
