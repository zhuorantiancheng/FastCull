# 构建与测试环境

事实：Rust 工具链不在全局 PATH，也不在 `~/.cargo`；项目把 rustup 工具链放在 `.agent-temp/2026-10-12-rust/toolchain/`（`cargo/` 为 CARGO_HOME，`rustup/` 为 RUSTUP_HOME，`cargo/bin/cargo.exe` 是 rustup shim）。构建必须在 shell 里显式设置这组环境变量，且 `CARGO_TARGET_DIR` 指向任务自己的 `.agent-temp/<日期>-<任务名>/target`，项目根不生成 `target/`。

适用条件：需要 cargo build / test / clippy / release，或复现历史验证记录时。

证据：`.agent-temp/2026-10-14-import-crop/build.ps1` 是现成模板（设置 CARGO_HOME、RUSTUP_HOME、PATH、CARGO_TARGET_DIR、CARGO_BUILD_TARGET=x86_64-pc-windows-msvc、私有 TEMP/TMP、`FASTCULL_RAW_DLL`，然后按 `-Action test|release|clippy|fmt|debug` 调 cargo）。2026-10-18 核对：`cargo` 不在 PATH，`~/.cargo` 不存在，`~/.rustup` 为空，`.agent-temp/2026-10-12-rust/toolchain/cargo/registry` 有依赖缓存。

补充：
- 2026-10-14 的基线：12 组 Rust 测试共 137 项通过，Lua/Python 合同测试 59 项通过（1 跳过），Clippy 无警告，release 构建约 30s。记录见 `.agent-temp/2026-10-14-import-crop/{rust-tests,lua-tests,clippy,release}.log`。
- Python 与原生模块另走 `scripts/build-native.ps1`、`scripts/build.ps1`、`scripts/build-release.ps1`；发布打包在 `scripts/release.py`。
- 本机 Python 用 `E:\APP\miniconda3\python.exe`，Node 用 `E:\APP\nodejs\node.exe`。

更新人：DSH（deepseek-flash），2026-10-18。
