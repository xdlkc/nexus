# Skills

可复用的能力包（Claude Code / Codex 等 agent 可直接加载的 `SKILL.md` 规范）。

| Skill | 说明 |
| --- | --- |
| [`ai-video-production`](./ai-video-production/SKILL.md) | 用「Claude 当总导演 + Codex 当画师 + Seedance 当动画师」的多模型协作流水线，把一句想法做成一支完整 AI 短片；含八步流程、可复制提示词与模型踩坑对照表。 |

## 目录结构约定

```
skills/<skill-name>/
  SKILL.md          # frontmatter(name, description) + 主流程
  references/       # 可复制提示词、对照表、清单等按需加载的详细资料
```

`SKILL.md` 保持精炼，把长提示词与对照表放 `references/`，只在需要时读取，避免一次性灌满上下文。
