# 构建与开发

Codex切号器的 Windows 原生实现。界面使用 Win32、Direct2D 和 DirectWrite，目标平台为 Windows 10/11 x64。

## 项目结构

| 路径 | 用途 |
| --- | --- |
| `src/` | 账号存储、额度查询、后台桥接、界面与在线更新实现 |
| `assets/` | 内嵌图标与图片资源，正式图标为 `app.ico` |
| `.cargo/config.toml` | Windows MSVC 静态运行库配置 |
| `Cargo.toml`、`Cargo.lock` | 项目配置与依赖锁文件 |
| `build.rs` | 图标与 EXE 版本资源构建 |
| `examples/` | 隔离测试使用的模拟后台及无界面测试程序 |
| `tests/` | 切号、连接、额度查询、更新及 DLL 依赖检查 |
| `package.ps1` | 构建并输出单文件 EXE |

## 构建

需要 Rust 的 `x86_64-pc-windows-msvc` 工具链、Visual Studio C++ 构建工具和 Windows SDK。在本目录执行：

```powershell
cargo build --release --locked
```

生成 `target/release/codex-switch.exe`。图标和版本信息内嵌在 EXE 中，用户无需额外安装界面或 MSVC 运行库。

将成品输出到仓库根目录并命名为 `Codex切号.exe`：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\package.ps1
```

原文件被占用时，脚本另存为带版本号的 EXE，不结束正在运行的进程。

## 测试

测试需要 Python 3。首次取得 Cargo 依赖后，可离线执行：

```powershell
cargo test --offline --locked
cargo build --offline --locked --bin codex-switch --examples
python -B tests/test_seamless.py
python -B tests/test_connection.py
python -B tests/test_usage.py
python -B tests/test_update.py
```

测试使用虚构账号、隔离目录、本机 HTTP 服务和模拟子进程。界面测试使用实际绘制代码的离屏目标，覆盖大小面板、弹窗、不同 DPI、点击区域及吸顶状态；不运行真实账号操作。

可选导出界面预览：

```powershell
$env:CODEX_SWITCH_UI_RENDER_DIR = Join-Path (Get-Location) 'target\ui-preview'
cargo test --offline --locked
```

可选使用已安装的原版后台验证认证切换，仍使用虚构凭据和本机 HTTP 服务：

```powershell
python -B tests/test_seamless.py --real-cli "C:\path\to\original\codex.exe"
```

发布构建的 DLL 导入检查（不执行 EXE）：

```powershell
python -B tests/check_portable.py target/release/codex-switch.exe
```

## 后台桥接

程序启动时检查后台连接。已就绪且协议兼容时复用现有桥接器；首次接入或确实缺失连接时自动接通，必要时重新打开 Codex。短暂状态超时不会触发客户端重启。

桥接程序保存在 `%LOCALAPPDATA%\CodexSwitch\bridge\`，通过 `CODEX_CLI_PATH` 接入原版后台，不修改 Codex 安装文件。连接就绪依据 `initialize` 成功响应。

正常切号通过原版后台认证接口执行，核对后台实际认证信息后提交本地凭据和账号列表。已有当前轮时排队，并暂缓新的 `turn/start`，待全部当前轮结束再切换。失败时回滚，正常切换保持客户端窗口和后台进程运行。

控制接口只监听随机的 `127.0.0.1` 端口，并要求随机访问密钥。多个后台同时使用同一 Codex 目录时拒绝切换。

## 常用命令

以下命令以构建产物为例，不带参数时打开图形界面：

```powershell
.\target\release\codex-switch.exe env
.\target\release\codex-switch.exe id
.\target\release\codex-switch.exe register
.\target\release\codex-switch.exe import "C:\path\to\auth.json"
.\target\release\codex-switch.exe list
.\target\release\codex-switch.exe usage
.\target\release\codex-switch.exe switch <ID或邮箱> --dry-run
.\target\release\codex-switch.exe switch <ID或邮箱>
.\target\release\codex-switch.exe seamless-status
.\target\release\codex-switch.exe cancel-switch
```

托盘右键菜单只提供「退出」。

## 存储与测试隔离

默认数据目录为 `%LOCALAPPDATA%\CodexSwitch\`，包含账号列表、凭据快照、界面设置、更新设置和桥接运行文件。

| 环境变量 | 用途 |
| --- | --- |
| `CODEX_HOME` | 当前 Codex 登录与配置目录 |
| `CODEX_SWITCH_DATA_HOME` | 切号器数据目录 |
| `CODEX_SWITCH_SETTINGS_FILE` | 用独立 JSON 文件替代用户环境设置，供隔离测试使用 |
| `CODEX_SWITCH_UI_FILE` | 界面设置文件 |
| `CODEX_SWITCH_DESKTOP_EXE` | 测试使用的桌面程序 |
| `CODEX_SWITCH_REAL_CLI` | 测试使用的后台程序 |

额度查询为各账号创建独立 profile 子进程，共用受控并发池，不修改全局环境或当前客户端。测试凭据必须为虚构数据。

## 兼容范围

- 无感切换针对已接入桥接器的本机 Desktop 后台；不切换云端任务、远程主机或其他独立 CLI 进程。
- 等待所有当前轮结束后切换；长期运行的任务会延长排队时间。队列保存在后台内存中。
- 浏览器登录和托管令牌刷新由原版 Codex 完成。
- 后台认证接口仍可能随 Codex 更新变化；接口不兼容时停止切换并报告错误。
