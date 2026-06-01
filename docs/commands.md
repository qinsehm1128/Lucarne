# Lucarne 命令参考

## Telegram

### Entry / 通知 topic

| 命令 | 用途 |
|---|---|
| `/panel` / `/start` | 打开或刷新管理面板 |
| `/help` | 查看入口帮助 |
| `/config` | 查看全局配置 |
| `/config global bypass\|notifications on\|off` | 开关全局 bypass / 通知 |
| `/status` | 查看全局 Agent 资源状态 |
| `/kill all\|<session_id:pid>` | 全局终止 Agent 进程 |
| `/clear_workspaces` | 清空 workspace 记录 |
| `/reset_notifications` | 重建通知 topic |
| `/aN` | 用面板第 N 个 agent 新建 session |
| `/hN` | 恢复当前页第 N 条历史 session |
| `/wN` | 打开当前视图第 N 个 workspace |

隐藏兼容输入：`/refresh`、`/next`、`/prev` 仍可手输或由按钮路径触发，但不注册到 Telegram BotCommand，也不作为公开命令展示。

### Workspace topic

| 命令 | 用途 |
|---|---|
| `/help` | 查看 workspace 命令帮助 |
| `/rename <name>` | 重命名当前 workspace |
| `/config workspace\|session bypass\|notifications on\|off` | 设置 workspace / session 级配置 |
| `/commands` | 列出当前 Agent 支持的命令 |
| `/commands <command>` | 通过 Lucarne 调用 Agent 命令 |
| `/commands <command> help` | 查看某个命令帮助 |
| `/model [model] [reasoning]` / `/models` | 查看 / 切换模型和推理档位 |
| `/permissions [mode]` | 查看 / 设置权限模式 |
| `/skills` | 列出可用 skills |
| `/status` | 查看当前 workspace Agent 状态和进程资源 |
| `/interrupt` | 中断当前 turn（绕过队列） |
| `/kill all\|<session_id:pid>` | 终止当前 workspace Agent 进程 |
| `/fork [target]` | 列出 fork 目标或 fork 指定目标 |
| `/fN` | fork `/fork` 列表中的第 N 个目标 |
| `/new` | 新建 Agent 对话 |
| `/quit` | 关闭当前 live session |

## WeChat

| 命令 / 操作 | 用途 |
|---|---|
| 引用 Lucarne 通知并回复 | 恢复对应 provider session，继续上下文 |
| 引用 Lucarne 通知并回复 `/new` | 为对应 workspace 新建 Agent 对话，后续回复接续新 session |
| 直接发普通消息 | 提示先引用通知 |
| `/status` | 查看全局或单 workspace 状态 |
| `/new`（未引用） | 提示先引用通知，避免无法确定 workspace |
| `/kill all` | 终止所有 Agent 进程 |
| `/kill <session_id:pid>` | 终止指定 Agent 进程 |
| `/help` | 查看 WeChat 命令帮助 |
| `/config` | 查看当前 bypass、notifications 状态 |
| `/config global notifications on\|off` | 开关全局通知 |
| `/config global bypass on\|off` | 开关全局权限绕过 |
