# Git 版本管理

本项目使用本地 Git 管理源码、测试、构建脚本、正式文档和验证记录。当前主分支为 `main`，包含 Windows 功能和新增的 macOS 适配源码。提交署名在本仓库设置为 `zhuorantiancheng <zhuorantiancheng@localhost>`；该邮箱只是本地标识。远程仓库为 `origin`（`https://github.com/zhuorantiancheng/FastCull.git`），推送 `main` 与 `v*` 标签会触发 GitHub Actions 的 Windows / macOS 构建工作流。

## 已保存的历史

| 引用 | 内容 |
| --- | --- |
| `main` | 纳入 Git 时的完整项目源码，包括尚未重新发行的修改 |
| `archive/releases` | 从已有 2.7.1～2.7.4 源码 ZIP 导入的快照链 |
| `archive/windows-2.7.1`～`archive/windows-2.7.4` | 对应发行版源码快照标签 |
| `archive/pre-git-backups` | 原 `backups/` 中的源码、文本备份，以及旧的 `dist/build-manifest.json` |

归档分支保留备份原来的相对路径，导入后逐文件核对 Git blob 与原文件字节。发行版快照仅包含当时源码 ZIP 提供的文件，不代表过去实际存在的 Git 提交历史，也不代表整个开发目录。归档提交时间是本次导入时间，没有伪造过去的作者或日期。

可通过 `git log --all --oneline --decorate` 查看记录；通过 `git ls-tree -r --name-only archive/pre-git-backups` 查询备份文件；通过 `git show 引用:相对路径` 查看某个历史文件。恢复源码前先确认当前工作已提交，避免覆盖未提交修改。

## 文件保留规则

- Git 跟踪 `Cargo.lock`、`vendor/LibRaw-0.22.2.zip`、测试夹具和正式验证截图，确保源码依赖与证据完整。
- `dist/`、`target/`、`.agent-temp/`、Python 缓存、插件内的编译程序、旧程序副本和本地配置不进入日常提交。
- `backups/` 被忽略；本次导入的旧源码备份已放在专用归档分支。以后修改源码以提交留档，通常无需再复制整份工程或重命名 exe 备份。
- `.gitattributes` 固定跨平台文本行尾，Shell 脚本使用 LF，Windows CMD／PowerShell 脚本使用 CRLF。
- `dist/public-2.7.4/` 是当前保留的既有发行包，不等于 `main` 的最新构建；后续发布应更新版本后从对应 Git 提交重新构建。

日常流程是先查看 `git status` 和 `git diff`，暂存本次要提交的文件，再执行 `git commit`。已有安装包不能代替源码提交；创建新版本标签前完成相应测试和打包。

Study 目录由 Seafile 双机同步。目前根目录同步忽略配置没有显式排除本项目的 `.git/`；两台电脑不要同时修改或同步尚未完成的 Git 元数据。若以后配置远程 Git，应明确源码同步与 Seafile 的分工，避免并发更新同一仓库数据库。
