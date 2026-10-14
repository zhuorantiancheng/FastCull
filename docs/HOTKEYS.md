# Bridge / Lightroom 快捷键

统一使用 **Ctrl + Alt + K** 打开 FastCull。先在 Bridge 或 Lightroom 中选中照片，再按组合键。文件夹选择沿用 Bridge 入口的递归加载规则；LR 入口仍读取目录评分并保留原有同步会话。

更新后重启 Bridge，在 Lightroom 增效工具管理器中重新载入 FastCull。Bridge 启动脚本直接启动同目录快捷键助手；LR 助手随插件初始化，不需要额外安装。

`fastcull-keys.exe` 是随插件分发的原生 Rust 小程序，使用 Windows `RegisterHotKey`。只有对应 Adobe 主窗口位于前台才注册 Ctrl+Alt+K，切换到其他软件时释放。检查周期为 40 ms；它不安装键盘钩子，不记录输入，不设置 Windows 开机自启。按住组合键不重复启动，800 ms 内重复请求合并，超过 1.5 秒的未处理请求丢弃，避免宿主忙碌后突然打开旧选区。

LR 卸载插件时停止助手；宿主非正常退出后，心跳超过 30 秒也会自动退出。两个宿主分别使用单实例助手。若其他软件已全局占用此键，助手不会替换其绑定，会在请求目录的 `status` 文件记录 `conflict`。Bridge 的状态目录为 `%APPDATA%/FastCull/hotkeys-bridge`；LR 的状态目录在插件会话目录旁的 `hotkeys-lightroom`。

实现通过请求文件调用原菜单函数，不用模拟菜单点击，也不直接修改 Adobe 快捷键配置。Bridge 热加载时取消旧定时任务，避免重复监听。无需 AutoHotkey。

Bridge 还会监测助手每秒更新的 `helper-alive` 心跳。宿主长时间忙碌导致助手退出后，定时任务恢复时自动重启助手；首次启动失败也会重试。恢复检查每 5 秒进行一次，启动尝试间隔至少 10 秒，健康时不启动额外进程。旧 `status` 文件不能单独证明助手仍在运行。

验证包括 Windows 原生快捷键注册、宿主进程名限制、Bridge 真实定时任务与请求分派，以及 LR SDK 模拟环境中的新请求、过期请求、停止处理。实际按键与用户当前 LR 选区的端到端操作仍需重新载入插件后试用。
