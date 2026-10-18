# 工作区 Windows 文件权限（DSH 沙箱）

事实：2026-10-18 本工作区（`E:\documents\Study\codes\IEDT`）在 DSH 沙箱里每次启动 pwsh 都报 `SetNamedSecurityInfoW failed (Win32 5): grantWrite(E:\documents\Study\codes\IEDT)`，所有命令不可用。原因是该目录缺少当前用户的 WRITE_OWNER（WRITE_DAC 已有），DSH 无法完成工作区授权写入。按 `diagnose-windows-sandbox-acl` skill 跑一次诊断脚本（unconfined），给目录加上当前用户 FullControl 后，pwsh 恢复正常。

适用条件：再次出现 pwsh 启动即失败、工作区读写被拒或目录列不出来时。先看恢复记录，不要手工改权限。

证据：诊断报告 `.agent-temp/2026-10-18-acl/acl-report-11c63c62aee0434aa0e0eee75fcf2dbb.jsonl`（verdict PRECONDITION，grant 后 writeOwner true，verification verified）；恢复脚本 `.agent-temp/2026-10-18-acl/acl-backup-6d5772f399cc4916941a778e1f0f85d3.json.ps1`（回滚用，别删）。修复后确认 `git status`、目录列举、构建日志读取均正常。

更新人：DSH（deepseek-flash），2026-10-18。
