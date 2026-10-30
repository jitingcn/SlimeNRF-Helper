# 开发与打包

[English](DEVELOPMENT.md)

[首页](../README.zh-CN.md) · [用户指南](USER_GUIDE.zh-CN.md) · [架构](ARCHITECTURE.zh-CN.md) · [协议来源](PROTOCOLS.zh-CN.md)

以下命令均从仓库根目录执行。工程规则以[根 AGENTS](../AGENTS.md)和[记录内核 AGENTS](../crates/recording/AGENTS.md)为准；本页维护操作步骤，不保存历史测试数量或工作站测量。

根清单是虚拟 workspace。默认桌面应用及其源码、构建支持位于 `crates/app`；内部库包括 `crates/recording`（`slimenrf-recording`）、`crates/terminal-core`（`slimenrf-terminal-core`）和 `crates/firmware-image`（`slimenrf-firmware-image`）。终端内核仅依赖标准库；固件镜像内核解析并校验镜像内容，不访问硬件或运行时。`crates/tools`（`slimenrf-tools`）提供共享 Rust 版权材料库和独立维护 CLI；应用构建只使用关闭 CLI feature 的库。GPUI 表示、资源、设备授权、传输和生命周期仍归应用所有。共享配置、翻译、脚本、锁文件、工具链和输出目录仍保留在仓库根目录。

命令、行为或链接变更时，同步更新默认英文文档及其同目录的 `.zh-CN.md` 对应文档。导航和跨文档链接保持当前语言。

## 工具链与构建模式

原生开发使用 [rust-toolchain.toml](../rust-toolchain.toml) 固定的 stable Rust（含 rustfmt、Clippy、rust-analyzer、rust-src），rustup 在本目录自动选择，不需改变全局默认。[Cargo.toml](../Cargo.toml) 中的 workspace 最低 Rust 版本跟随固定编译器的次版本；开发和 CI 验证固定的补丁版本，不代表验证了兼容下限之上的每个编译器版本。各成员继承共享的作者、仓库和主页信息，同时保留各自的描述。依赖版本以 [Cargo.lock](../Cargo.lock) 为准，GUI 通过同源 `gpui_kit` 导出使用官方 Fast feature。

脚本 test 操作和非纯格式质量检查需要安装 cargo-nextest：`cargo install --locked cargo-nextest --version 0.9.150`。脚本不会自动安装；build、check、纯格式和显式的纯 doctest 操作不需要它。

应用构建、脚本、直接 Cargo 检查与测试、编辑器分析、打包、CI 和独立 oracle 均不需要 Python。构建脚本和打包共用同一个 Rust 版权材料生成器；维护工具通过独立的 `slimenrf-tools` 包运行。下文各流程所需的原生及外部输入仍不可省略。

CLI 应用及 workspace 构建默认仍启用 `embed-firmware`。独立 checkout 没有固件载荷时，所有选择应用的 CLI 构建、运行、检查、测试和打包命令明确加 `-NoFirmware` 或 `--no-firmware`，直接 Cargo 用 `--no-default-features`。完整构建必须提供预期固件，不能用无固件包代替完整包验收。

脚本支持 build/run/test/check/clippy/fmt/clean：build 默认 Release，run/test/check/clippy 默认开发配置；显式 Debug/Release 互斥。默认 build/run 选择桌面应用，test/check/clippy 覆盖 workspace，fmt 覆盖全部成员；显式 Cargo 包选择优先。开发 profile 中应用使用优化级别 0，recording 和其他依赖使用级别 1；Cargo 默认的 build override 仍不优化。Release 使用优化级别 2、fat LTO、单个 codegen unit，并剥离符号。权威配置见 [Cargo.toml](../Cargo.toml)；package 只取实际 Release 产物，不用开发版替代。

Cargo 默认根据可用 CPU 自动选择构建并发数，可用 `CARGO_BUILD_JOBS` 覆盖。测试操作中 nextest 的 `--build-jobs` 控制构建并发，而 `-j` / `--jobs` 控制测试执行并发。

当精确的 Git 标签 `v<Cargo 版本号>` 指向 HEAD 时，干净 checkout 只显示 Cargo 版本，如 `0.2.0` 或 `0.2.0-rc.1`。轻量标签和附注标签均支持；GitHub 的预发布标记不会额外添加 hash。其他开发 checkout 显示 `0.2.0+abcdef0`。本地已跟踪文件存在变更时，即使匹配发布标签，也保留 `0.2.0+abcdef0.dirty`；被忽略和未跟踪的文件不会使其变为 dirty。CI 忽略工作区 dirty 状态，优先使用实际 checkout 的 HEAD。无法取得 Git 元数据时，源码归档显示基础版本，若有经过校验的 GitHub Actions SHA 则添加该 SHA 后缀。此显示标识不改变发布标签、产物文件名或更新版本比较。

增量构建时，Cargo 逐项跟踪 Git 元数据和已跟踪的输入，而不是监视整个 checkout，也会跟踪匹配发布标签的创建、移动、删除和打包。已删除的已跟踪输入在缺失期间可能使构建脚本反复运行；恢复后，未变化的构建可再次保持 fresh。

<a id="windows"></a>
## Windows

安装 Visual Studio 2022“使用 C++ 的桌面开发”、Windows SDK、CMake 和 libclang（例如来自 LLVM）。Windows MSVC target 使用 Rust 工具链自带的 `rust-lld`，无需另装 LLVM 链接器，但仍需要 libclang。PowerShell 入口初始化并恢复调用者环境和工作目录，使用 VS CMake、显式 `x86_64-pc-windows-msvc` target 与静态 MSVC CRT，保留已有 Rust flags；直接 Cargo 要自行配置这些条件，不能只对 host 添加 crt-static 而影响过程宏。

在编辑器的开发终端中重复初始化时，脚本保持静态 CRT 参数和工具搜索路径稳定，不再因累积重复条目使 Cargo 缓存失效。日常编辑与运行应复用相同的固件模式和 target 目录；测试会启用额外的 GUI 支持，可能需要另一组依赖构建。Cargo 的“等待文件锁”耗时来自其他构建占用，不是编译器耗时。

```powershell
./build.ps1 -Action run -NoFirmware           # 开发编译并启动
./build.ps1 -Debug -NoFirmware                # 开发编译，不启动
./build.ps1 -Action check -NoFirmware
./build.ps1 -Action test -NoFirmware
./build.ps1 -Action run -Release -NoFirmware  # 明确运行 Release
./package.ps1 -NoFirmware                    # 单文件发布包
./quality.ps1 -NoFirmware                    # 格式 -> 严格 Clippy -> 测试
./quality.ps1 -FormatOnly                    # 只检查，不改格式，无需 VS
./build.ps1 -Action fmt                      # 明确应用格式
```

需要策略隔离时用 `powershell -NoProfile -ExecutionPolicy Bypass -File ./build.ps1 ...`（其他脚本同理）。有固件输入时去掉 `-NoFirmware`。附加参数用 `-CargoArguments`，例如 `./build.ps1 -Action test -CargoArguments @('-p', 'slimenrf-recording')`。测试参数采用 nextest 语义：`-P` / `--profile` 选择 nextest profile，`--cargo-profile` 选择 Cargo profile。

Nextest 在 `--` 之后只支持其模拟的 libtest 参数：`--ignored`、`--include-ignored`、`--skip`、`--exact` 和 `--nocapture`。尤其不要在 `--` 后传 libtest `--test-threads`；应在其之前使用 nextest 的 `-j` / `--test-threads`。使用 `--doc` 时改用 Cargo 和 libtest 参数，不要混入 nextest 专用选项。

正在运行的 EXE 可能锁住输出；先正常退出自己的实例。确需保留时可设置 `$env:CARGO_TARGET_DIR='target/verified'`，相对路径以仓库根目录解析；输出在其 `x86_64-pc-windows-msvc/{debug,release}/`，不要为编辑器另建缓存。

<a id="checks-and-editor"></a>
## 检查与编辑器

`quality.ps1` 和 `quality.sh` 使用锁定依赖、stable rustfmt、整个 workspace/all-target 严格 Clippy `-D warnings`、nextest 和单独的 Cargo doctest，失败即停止。共享边界、构建、配置或资源变更应覆盖完整和无固件两种模式；不要加 `--all-features` 重新开启内置固件。

配置好原生依赖后，等价命令：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --all-targets --locked
cargo test --workspace --doc --locked
```

无固件模式为 Clippy、nextest 和 doctest 添加 `--no-default-features`。脚本 test 操作只运行 nextest；使用 `./build.ps1 -Action test -CargoArguments @('--doc')` 或 `bash ./build.sh test --cargo-arguments --doc` 单独通过 Cargo 运行 doctest。纯 doctest 操作使用 Cargo 参数语义。

轻量维护 CLI 及其测试也可在没有 GPUI 或固件输入时单独运行。公开检查及 hook 测试需要 Git；运行平台专用 hook 测试还需要原生 shell（Windows 使用 PowerShell）：

```text
cargo nextest run --locked -p slimenrf-tools
```

无固件 workspace 门槛之外还须显式运行此工具测试：`--workspace --no-default-features` 同时会关闭工具包默认的 CLI feature。

单独处理内核时可明确选择包；`cargo test` 包含其单元测试和 doctest，`cargo doc --no-deps` 构建其 API 文档：

```text
cargo check --locked -p slimenrf-recording
cargo test --locked -p slimenrf-recording
cargo doc --locked --no-deps -p slimenrf-recording

cargo check --locked -p slimenrf-terminal-core
cargo test --locked -p slimenrf-terminal-core
cargo doc --locked --no-deps -p slimenrf-terminal-core

cargo check --locked -p slimenrf-firmware-image
cargo test --locked -p slimenrf-firmware-image
cargo doc --locked --no-deps -p slimenrf-firmware-image
```

如对单个内核使用 nextest，以 `-p` 选择，例如 `cargo nextest run --locked -p slimenrf-recording`，并用 `cargo test --locked --doc -p slimenrf-recording` 单独运行 doctest。记录内核仍需 C 编译器。终端和镜像库不需要应用构建脚本、GPUI、硬件接口、应用 YAML 或固件载荷。这些定向命令不替代适用的完整和无固件 workspace 覆盖。外部 updater fixture 与完整固件 oracle 为有真实额外输入的独立检查，不把 ignored 当通过。

创建 `Workbench` GUI 测试 fixture 前，在测试函数的最外层创建一个 [`crate::test_support::UiTestSession`](../crates/app/src/test_support.rs)。同一 `TestAppContext` 的所有 fixture 共用这个会话。让会话保持到回调和断言结束，以隔离进程级语言状态。各能力的测试与其所有者放在一起，参见[源码与命名空间边界](ARCHITECTURE.zh-CN.md#依赖方向与模块)，不要重新集中到全局 UI 文件夹。

在 `#[gpui_kit::test]` 之外创建上下文时，遵循 [GUI 测试 fixture](../crates/app/src/application/test_support.rs)中的独立调度器所有权顺序。先销毁上下文，再清理 `TestDispatcher` 的待处理任务，期间保持调度器存活。让 `AppRuntime` 保持到 UI 清理结束。

`.vscode` 只维护项目级设置。rust-analyzer 使用正常 `check`，并启用 `rust-analyzer.cargo.noDefaultFeatures`，因此编辑器分析不需要固件载荷。此默认值只影响编辑器，不改变 CLI feature。严格 Clippy 在显式质量入口执行。Windows 关闭所有 VS Code 后可运行 `./scripts/open-editor.ps1`，复用 MSVC/CRT 环境与正常缓存；脚本拒绝复用旧 Code 进程，不关闭别人的窗口或改全局设置。

保持已固定的 stable 工具链、正常 rust-analyzer `check` 和现有原生缓存。Windows 上调用 Cargo 的工具需要继承一致的宿主 MSVC、C 和 C++ 环境。环境差异可能使原生构建指纹失效，即使源码没有变化。仅凭指纹失效不能证明源码陈旧，也不能据此例行 `clean`。

可选 `./scripts/install-hooks.ps1` 仅在应用本身已是独立 Git 根目录时安装非修改式格式 hook，拒绝父仓库、保留其他 hooks、不覆盖不同的 pre-commit。虚拟 workspace 迁移前已安装的副本使用旧标记，不会自动升级；安装当前 hook 前须自行审查并手动替换旧副本。脚本不自动暂存或提交。hook 不替代质量入口或原生 GUI 验证；部分暂存需自行确保已检工作树与暂存内容一致。

<a id="device-diagnosis"></a>
## 设备诊断

从实际症状和当前设备列表入手，区分设备缺失、无权限、被占用、身份不明确和枚举失败。操作参见[连接帮助与设备使用](USER_GUIDE.zh-CN.md#first-use)、[Linux 设备要求](USER_GUIDE.zh-CN.md#linux-devices)，解释参见[架构](ARCHITECTURE.zh-CN.md)和[协议来源](PROTOCOLS.zh-CN.md)。不要仅凭端口号或追踪器槽位推断身份；保留现有读取器、预留、身份、租约、取消与 join 的边界。

应用正常启动会枚举设备并以只读方式监听主 HID 接口。可以浏览首页和连接指南而不发送命令，但打开 ESB 更新页面会自动发起只读固件查询。HID 页面的浏览和筛选不会启动原始采集；显式订阅会发送能力查询与订阅请求，录制会写入文件。`.hidlog` 只包含本应用的 HID 流量，并非系统级 USB 或串口日志，详见[用户指南](USER_GUIDE.zh-CN.md)。

发送命令、订阅、录制、校准、OTA 或刷写前须获得明确且范围适当的授权；不要仅为 UI smoke 执行这些操作。仅私下收集必要证据，分享前脱敏设备标识、私有路径、凭据和录制内容。UI 观察不能代替硬件验收。

<a id="developer-gallery"></a>
## 开发者画廊

仅 debug 构建提供画廊：以合成的内存状态独立于正常工作台使用生产展示组件。

```powershell
./build.ps1 -Debug -NoFirmware
./target/x86_64-pc-windows-msvc/debug/slimenrf-helper.exe --gallery
```

```bash
bash build.sh --debug --no-firmware
./target/debug/slimenrf-helper --gallery
```

路径使用默认 target 目录；设置 `CARGO_TARGET_DIR` 时相应调整。有预期固件输入时可去掉无固件选项。Release 构建拒绝 `--gallery`，普通启动不变。

Controls 场景提供可选择的合成卡片、短/长内容和动态详情；Terminal 场景使用有界合成终端缓冲与文本，提供追加、清空、跟随和换行控制。切换场景、中英文、浅色/深色和正常/1.25 倍文字，调整窗口时检查选中状态与布局（默认 1100×760、最小 760×600 逻辑像素）。

画廊在 `AppRuntime` 前分支，不创建 `Workbench`，不读取或保存偏好，不启动目录、发现、串口、HID、BLE 或固件任务，也不发送设备命令。它不是设备模拟器，不能证明正常工作台、原生文件选择器或硬件行为。无头 GUI 回归测试、原生画廊观察、正常应用 smoke 和经授权的硬件验收是不同证据。

<a id="native-ui-smoke"></a>
## 原生 UI smoke

按上述平台说明（以及 [Linux 前置条件](#linux)）构建实际应用，选择预期的固件模式，记录二进制身份、平台、窗口大小、操作和观察结果。根据变更覆盖窄窗口、放大文字及中英文；检查实际渲染和交互，而不只是进程启动。分别报告测试、原生桌面、私有虚拟显示/WSL 和实体硬件证据，以及未覆盖的部分。只操作自己启动的进程和窗口，正常退出，不终止其他实例。

- **Windows：**启动自己的进程，取得 PID 和实际主窗口 HWND；采集或窗口内输入前确认 HWND 属于该 PID。使用 Windows.Graphics.Capture 等窗口级采集并检查图像。不要仅凭标题选窗或发送全局输入。仅修改 `APPDATA` 不能保证 Windows KnownFolder 偏好隔离；保护真实偏好，不要用旧备份覆盖。
- **Linux：**隔离运行使用私有显示及私有 XDG 配置/状态目录，并准备可用的 Vulkan ICD、字体和用户 D-Bus。只清理自己启动的显示、总线和应用进程，观察实际渲染器和窗口。测试原生文件选择器时，在同一私有总线与显示上运行真实 portal 和兼容后端，并操作实际选择流程。私有 Xvfb、WSLg 和原生桌面验收应分别报告；它们都不能证明 USB、BLE 或刷写功能。

私有 Linux 运行除构建依赖外还需安装 Xvfb、`xvfb-run`、`dbus-run-session`、Vulkan ICD 和字体。为 `XDG_CONFIG_HOME`、`XDG_DATA_HOME`、`XDG_STATE_HOME` 和权限为 0700 的 `XDG_RUNTIME_DIR` 创建新的私有目录并导出变量，在新的 `dbus-run-session` 和 `xvfb-run` 下启动应用。覆盖文件选择器时还须安装 `xdg-desktop-portal` 与兼容桌面后端，确保服务使用同一会话总线和显示；结束自己启动的会话前检查真实对话框。模拟实现不能充当原生选择器证据。

绘制或滚动诊断参见单次启动的 [Fast 开关](#clean-and-troubleshoot)。不要假设缓存的 wheel、特定机器脚本、显示编号或私有产物路径存在。证据私下保存，分享前脱敏。

## 更新依赖

1. 修改版本要求前，检查上游发布说明和清单中的 MSRV。保持普通依赖与测试依赖的 `gpui-kit` 版本要求一致。
2. 同步更新清单与 `Cargo.lock`，使用定向的 `cargo update -p PACKAGE --precise VERSION`，不要无范围限制地更新全部依赖。更新 GPUI 时检查已发布的 Kit 清单，并同步更新完整的 Kit 与 Fast 系列；当前版本要求为 Kit 0.8.1，锁文件选择 Fast 0.1.12。只通过 `gpui_kit` 导出使用同一套 GPUI 类型。在运行下列检查前，审查上游 API 变更，特别是修饰键处理及自定义 `Element` 的布局、prepaint、paint 生命周期。
3. 按平台运行[质量检查命令](#checks-and-editor)，覆盖完整固件和无固件两种模式。
4. 运行原生应用，检查终端滚动、自动换行、文本选择、图表交互和远程固件加载。
5. 根据两个平台锁定的依赖图刷新[许可证声明](#license-notices)。

应用向 reqwest 显式提供内置的 Mozilla 根证书。修改 TLS 依赖时保留这一策略，离线启动不能依赖系统证书库。HTTPS 仍然校验证书链和主机名。

<a id="translations"></a>
## 修改界面翻译

修改文案时保留现有键名。新增文案时按以下步骤操作。

1. 按功能和用途选择键，例如 `firmware.serial_dfu.enter`。不要用整句英文作键。同样的英文若表示不同内容，使用不同的上下文键。
2. 在 [locales/en.yml](../locales/en.yml) 和 [locales/zh-CN.yml](../locales/zh-CN.yml) 中添加相同的 YAML 层级。层级片段使用小写字母、数字和下划线，以字母开头。叶节点保存文案，不要再拆功能文件。
3. 保持各语言的 `%{name}` 占位符名称一致。保留有意义的换行。
4. 用 `tr("firmware.serial_dfu.enter")` 读取普通文案。需要替换占位符时使用 `rust_i18n::t!`。
5. 直接显示命令值、设备输出、设备名称和用户预设名称，不要把这些数据传给翻译函数。若要翻译内核错误，在应用层映射到键，不要给内核库添加翻译依赖。
6. 重新构建应用。仅修改 YAML 也会触发编译，无需触碰 Rust 文件或清理缓存。
7. 运行[质量检查](#checks-and-editor)的完整和无固件模式。随后切换英文与简体中文，检查修改的界面、输入提示和已打开的工具窗口，并确认已输入的值保持不变。

翻译资源测试检查语言键是否一致、嵌套键是否重复、占位符是否一致，以及编译后的查找和英文回退。格式与职责见[翻译归属](ARCHITECTURE.zh-CN.md#translation-ownership)。

## 配置、资源与源码卫生

权威配置见 [slimenrf-helper.yaml](../slimenrf-helper.yaml)。`schema_version`、板型、精确资源/alias、OpenDFU 参数和远程 application/bootloader 命名空间在此维护，不复制另一份配置表。

- 开发默认 `firmware_root: ../firmwares`，仓库外目录不随 checkout 提供。资源相对于所选 YAML 所在目录解析。
- 构建用 `SLIMENRF_CONFIG_PATH` 显式选择 YAML，用 `SLIMENRF_FIRMWARE_ROOT` 提供本机固件根目录；这两个变量的相对值均以仓库根目录为基准，YAML 中的 `firmware_root` 仍相对于该 YAML 所在目录解析。公开和嵌入 YAML 保持可移植相对路径。无固件构建不读取固件根目录但仍校验配置。
- 运行时按可执行文件目录 → 工作目录 → 内置配置选择 `slimenrf-helper.yaml`；不读旧 Python 配置、不搜索父目录。外部配置权威，无效或缺失资源直接报错，不静默混入内置来源。构建输入变量不等于运行时自动加载任意私有 YAML。
- `firmware` 和 `bootloader` 是精确默认资源，缺失不换版本。显式 aliases 只允许受限版本片段匹配；目录仅在加载时扫描顶层，候选不自动选最新或覆盖用户选择。
- 完整包只嵌入配置和固件根目录顶层受支持 UF2/HEX/BIN/ZIP，不打包开发工作区、日志或备份。资源运行时单成员有界解压、大小/CRC/SHA 校验，不在程序旁释放整个目录。文件与内置成员的来源身份不能按 basename 混用。

本地配置、录制、日志、凭据及历史执行材料放在忽略的 `.local/`；也可用精确忽略的 `slimenrf-helper.local.yaml`，但必须显式选择，应用和脚本不会自动读 `.local/`、`.env` 或 `*.local.yaml`。不提交真实 USB serial/RF 地址、令牌、私钥、私有绝对路径；公开问题样本另行脱敏。平台用户偏好不可为发布整理而搬动或覆盖。忽略规则不会从 Git 中移除已跟踪文件；维护者须另行审查实际发布内容及历史。

<a id="public-preflight"></a>
## 发布源码前检查

[公开检查器](../crates/tools/src/public.rs)需要 Git 和已固定的 Rust 工具链。应用必须有自己的 Git 根目录，可以是独立仓库或常规 linked worktree，不能借用父仓库。

```bash
cargo run --locked -p slimenrf-tools -- check-public
cargo nextest run --locked -p slimenrf-tools public::tests
```

检查器列出已跟踪文件、未被忽略的未跟踪文件，以及匹配忽略规则但仍被跟踪的文件。它检查工作树内容，不检查暂存字节或 Git 历史。禁止公开的路径只报告，不读取其内容。每个文本文件最多读取 8 MiB，使用 UTF-8，可带 BOM。二进制、无法解码、超限、不可读或其他未经审查的候选会使检查失败，符号链接和 reparse point 也会被拒绝。

检查应用根目录与 `.git` 不含符号链接或 reparse point 后，Git 根目录比较会解析路径别名，包括 Windows 8.3 短名与长名。候选路径遍历和显式 `--root` 匹配仍使用词法路径。Windows fixture 使用真实临时目录短路径，并报告是否覆盖了 Git 根目录不同写法；运行 `cargo test --locked -p slimenrf-tools windows_short_path_git_root_preserves_audit_guards -- --nocapture` 可查看状态。

退出码 `0` 表示没有发现问题，`1` 表示发现需要审查的问题，`2` 表示参数无效，或根目录、Git、运行条件出错。问题报告只含路径、行号和类别，不输出匹配到的秘密值。有限的已知模式和文档链接检查不能证明凭据、硬件身份、历史或发布产物已清理，仍需分别审查。检查器不暂存、不发布、不删除文件，也不改变用户状态。

<a id="license-notices"></a>
## 维护版权材料

[Rust 版权材料生成器](../crates/tools/src/notices.rs)由应用构建脚本、打包和维护 CLI 共用。先校验已跟踪输入与文本生成，不读取或写入生成文档，再运行工具测试：

```bash
cargo run --locked -p slimenrf-tools -- notices --check
cargo nextest run --locked -p slimenrf-tools
```

[scripts/notices-inputs.json](../scripts/notices-inputs.json)冻结经审查的可移植包元数据、目标依赖图、原始版权文本字节、来源和 SHA-256 哈希。这些输入、来源记录和仓库许可证源文件仍纳入版本控制。生成器核对输入哈希、锁定包校验和，以及包条目对冻结依赖图的覆盖情况。它仅从本地输入确定性生成 `THIRD_PARTY_NOTICES.txt`，不调用 Cargo、registry 缓存或网络，也不会发现依赖或收集许可证文本。`cargo run` 命令会先构建 CLI，可能需要 Cargo 可获取依赖；已构建的 CLI 可离线生成。

对于已批准的仅涉及 `Cargo.toml` profile 设置的变更，可刷新冻结输入哈希并校验结果：

```bash
cargo run --locked -p slimenrf-tools -- notices --refresh-inputs --check
```

语义指纹不包含 profile 设置；workspace 成员及依赖变更仍须按下述流程审查。不得借仅 profile 刷新绕过依赖或来源审查。

Cargo 输入、vendor 源码或许可材料变更时：

1. 审查变更输入。workspace 成员或依赖变更时，即使第三方版本不变，也用以下命令获取两个锁定目标依赖图。从 `slimenrf-helper` 沿 normal 和 build 边追踪，排除 dev 边。将 build 和 proc-macro 路径标为编译工具，保留已审查清单与生成器说明的保守并集和 feature 合并。

   ```bash
   cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
   cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-unknown-linux-gnu
   ```

2. 审查每个新增或变更的包及来源。按需更新冻结依赖图、包与其他材料条目、源码获取说明和输入哈希。保留已有原始来源字节，在输入结构中添加经审查的精确许可证与 NOTICE 字节及其来源、编码、字节长度和哈希。不要只替换哈希来消除输入过期错误。
3. 在公开源码清单之外生成本地审查副本。审查变化后，再次运行 `--check` 和生成器测试：

   ```bash
   cargo run --locked -p slimenrf-tools -- notices --output .local/THIRD_PARTY_NOTICES.txt
   cargo run --locked -p slimenrf-tools -- notices --check --output .local/THIRD_PARTY_NOTICES.txt
   ```

写入必须显式指定 `--output` 路径。单独使用 `--check` 只校验输入与文本生成，不需要输出文件。加上 `--output` 时，它逐字节比较指定产物，绝不重写文件。

`crates/app` 中的应用构建脚本在完整和无固件模式下直接调用同一个 Rust 库，写入 `OUT_DIR/THIRD_PARTY_NOTICES.txt` 并嵌入这些字节。应用以 Cargo `PROFILE=debug` 构建时，版权材料校验、生成或刷新提交失败不阻断 debug 构建、运行或测试：Cargo 会给出警告，版权页面显示 `DEVELOPMENT LICENSE REVIEW PENDING`，而不是过期的清单。输出文件系统错误仍会使构建失败。CLI 的 `--development` 模式同样写入待审查提示，不能与 `--check` 同用；release 和其他非 debug 构建、打包及 `--check` 仍严格要求输入经过审查且保持最新。

Cargo 逐项跟踪生成器、冻结输入及仓库来源文件。生成文档不纳入版本控制，不要提交根目录副本或重复实现生成器。AppImage 和 Flatpak 打包用同一个库产生安装副本。AppImage 将 `LICENSE-MIT`、`LICENSE-APACHE` 和 `THIRD_PARTY_NOTICES.txt` 安装到 AppDir 内的 `usr/share/licenses/slimenrf-helper`，Flatpak 将同样文件安装到 `/app/share/licenses/slimenrf-helper`。

按版权文档说明，另行保证 MPL-2.0 覆盖源码可获取。每次发布保留校验和一致的精确源码归档，或提供等效的稳定位置。修改覆盖文件后，提供实际修改的源码并更新说明。生成版权文档不代表取得固件或最终产物所含原生库、DLL、运行时和工具链材料的再分发权，每次发布仍需审查。

## Windows 发布

`package.ps1` 构建并发布 UPX 压缩的 `dist/slimenrf-helper-<版本>-windows-x64.exe`；`-NoFirmware` 发布 `-no-firmware.exe`，仍内置默认配置。Cargo 构建的未压缩原始文件保留在构建缓存中，不予发布；没有公开的未压缩备用版本。不发布整个 target、PDB 或测试程序。静态 CRT 不等于全新 Windows 或签名已验收。

```powershell
./package.ps1 -NoFirmware
```

`package.ps1` 以明确的必填 `-InputPath` 和 `-OutputPath` 调用内部 [UPX 辅助脚本](../scripts/package-upx.ps1)。该脚本固定工具版本与哈希，使用 `--best --lzma --strip-relocs=0`，不使用 `--exact`。发布前先运行 UPX 完整性检查，再解压到独立临时文件。解压后的可执行文件可能与原文件字节不同；Cargo 构建的输入保持不变。

压缩可能增加启动开销或引发安全软件误报。压缩改变 EXE 字节，已有 Authenticode 签名失效；若签名应在最终压缩后进行并重新验收。CI 不签名。

辅助脚本使用[共用原子发布器](../scripts/publish-file.ps1)。已有输出内容相同时保留，不同内容须在 `package.ps1` 明确传入 `-Force`。发布器在目标卷暂存，再移动或替换目标，不原地覆盖字节，因此保留输入及指向旧输出的其他硬链接。发布路径中的符号链接和 reparse point 会被拒绝。文件锁允许读取但拒绝写入，输入锁还拒绝删除。无法取得所需锁时，发布失败且不替换输出。这些检查不保证能抵御所有恶意文件系统竞态。

仅官方 tag 的 Windows 打包任务通过 `package.ps1 -OfficialPrereleaseTag` 写入应用自更新资格。普通本地、分支、PR、debug 和自定义资源构建只允许检查版本。[构建标记校验](../crates/app/build_support/official_release.rs)核对包版本、目标、profile、仓库、tag 及 GitHub 事件；这是分发策略，不是密码学签名。普通开发命令不要设置此标记。

更新器读取分页的 GitHub Releases API，而非 `/latest`，因为当前发布工作流生成预发布版本。Windows 产物必须保持上述精确的版本／架构／固件变体命名，并提供 GitHub 的 SHA-256 摘要；不会回退到历史 `-upx.exe` 名称或另一固件变体。仅检查版本的构建选择发布元数据，不要求存在 Windows 产物，并打开对应发布页。修改产物命名或发布频道时须同时审查更新器。用户流程和恢复限制见[应用更新](USER_GUIDE.zh-CN.md#app-updates)。

<a id="linux"></a>
## Linux

需要 C/C++、Clang/libclang、CMake、pkg-config、udev、D-Bus、xkbcommon/X11、Wayland、X11/XCB、fontconfig/FreeType；Rust 用本机 rustup 工具链，不注入 Windows target/CRT。

```bash
# Arch
sudo pacman -S --needed base-devel clang cmake pkgconf systemd-libs dbus \
  libxkbcommon libxkbcommon-x11 wayland libx11 libxcb fontconfig freetype2 \
  vulkan-icd-loader
# Debian / Ubuntu
sudo apt update
sudo apt install build-essential clang cmake pkg-config libudev-dev libdbus-1-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-dev libx11-xcb-dev \
  libxcb1-dev libfontconfig1-dev libfreetype6-dev libvulkan1

bash ./build.sh run --no-firmware
bash ./build.sh check --no-firmware
bash ./quality.sh --no-firmware
bash ./quality.sh --format-only
bash ./build.sh fmt
bash ./package.sh --no-firmware
```

完整 CI 依赖清单以 [workflow](../.github/workflows/ci.yml) 为准，上游参考见 [GPUI Kit 安装说明](https://gpui-kit.com/docs/installation)。运行需实际 X11/Wayland、Vulkan GPU 驱动/ICD、用户 D-Bus 与字体；可安装 DejaVu/Noto/CJK 字体。文件选择器需 `xdg-desktop-portal` 和对应桌面 backend（精简会话可用 gtk backend）；BLE 需 BlueZ/适配器。设备权限、真实 UF2 挂载与 WSL 边界见[用户指南](USER_GUIDE.zh-CN.md#linux-devices)。

附加 Cargo 参数用 `--cargo-arguments`，例如 `bash ./build.sh test --cargo-arguments -p slimenrf-recording`。`package.sh` 默认 ELF，使用 Rust 打包工具。打包根据 Cargo JSON 选择实际 Release 应用 ELF，按版本与 ELF 架构命名，无固件加后缀。同名冲突须明确 `--force`。ELF 并非静态通用包，应在计划支持的最旧 glibc 基线构建，再到目标发行版验收。

原生缓存默认 target；`CARGO_TARGET_DIR` 相对仓库根目录，显式 `--target-dir` 优先。不要混用 Windows/Linux/SDK 编译缓存；WSL 可将唯一 Linux 缓存放在 `$HOME/.cache/slimenrf-helper/target`，编辑器与脚本共用。

### 可选 Linux mold 链接器

Linux x86-64 GNU 构建通常使用 Rust 自带的 LLD。若要启用 [mold 配置](../.cargo/mold.toml)，安装 `clang` 和 `mold`，确保 `PATH` 中有 `ld.mold`，然后显式传入配置：

```bash
bash ./build.sh --debug --no-firmware --cargo-arguments --config .cargo/mold.toml
# 或直接使用 Cargo：
cargo --config .cargo/mold.toml build --no-default-features
```

该配置使用 Clang 和 `-fuse-ld=mold`，不改变 CI 或 Flatpak 默认值。Cargo 优先采用 `CARGO_ENCODED_RUSTFLAGS`，其次是 `RUSTFLAGS`，再使用配置的 target rustflags；设置任一环境变量都可能使 mold 参数失效，即使仍选择了 Clang。显式 target triple 的 linker 设置也可能覆盖 Linux cfg linker。切换链接器设置可能导致缓存产物重新构建。

### AppImage

除原生依赖还需 curl、`file`、`desktop-file-utils`；[packager](../crates/tools/src/packaging/appimage.rs) 固定 linuxdeploy/appimagetool 版本及 SHA-256，执行前校验，不接受可变 latest。x86-64 一次性准备：

```bash
mkdir -p "$HOME/.cache/slimenrf-helper/appimage-tools/1-alpha-20251107-1" \
  "$HOME/.cache/slimenrf-helper/appimage-tools/1.9.1"
export SLIMENRF_LINUXDEPLOY="$HOME/.cache/slimenrf-helper/appimage-tools/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage"
export SLIMENRF_APPIMAGETOOL="$HOME/.cache/slimenrf-helper/appimage-tools/1.9.1/appimagetool-x86_64.AppImage"
curl -fL https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage -o "$SLIMENRF_LINUXDEPLOY"
curl -fL https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage -o "$SLIMENRF_APPIMAGETOOL"
chmod +x "$SLIMENRF_LINUXDEPLOY" "$SLIMENRF_APPIMAGETOOL"
bash ./package.sh --format appimage --no-firmware
```

下载的工具与生成应用都支持 `--appimage-extract-and-run`。AppImage 携带选定桌面客户端库，不捆绑 glibc/loader、GPU/图形 loader、Wayland、libudev、字体或服务。Wayland 必须与宿主驱动 ABI 一致，部分 X11 Mesa 路径也会加载；不能只排除驱动文件却捆绑旧 Wayland。构建基线仍决定最低 glibc，入口保留工作目录及参数。linuxdeploy 二次 strip 关闭，应用自身仍按 Release 裁剪。

<a id="flatpak"></a>
### Flatpak

Flatpak 是可选的本地源码包，不是 host ELF 包装。需要 flatpak 和 flatpak-builder。完整包仍须提供预期固件输入；其 YAML 配置由 Rust 打包工具解析。

```bash
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user flathub org.freedesktop.Platform//26.08 org.freedesktop.Sdk//26.08
bash ./package.sh --format flatpak --no-firmware
flatpak install --user ./dist/slimenrf-helper-0.1.1-linux-x86_64-no-firmware.flatpak
flatpak run io.github.jitingcn.SlimeNRFHelper
```

[构建入口](../crates/tools/src/packaging/flatpak.rs)从同一 Rust pin 及官方发行清单产生 SHA 校验的构建专用 rustc/cargo/std 模块，在禁网 SDK 中 `--offline --locked` 编译，版本或架构缺失直接失败，不回退 SDK 编译器；导出前清理工具链。完整和无固件包使用同一应用 ID，安装相互替换。

SDK 构建通过同一个 Rust 库从暂存的已跟踪输入离线生成内嵌及安装版权材料。此构建的宿主和 SDK 均不需要 Python。

这是宽权限兼容包，不是严格沙箱。为 hidraw/tty/BLE/任意原生 UF2 挂载授予设备、host 文件、只读 udev、BlueZ 指定总线、网络及图形权限，仍受宿主 ACL 和块设备身份检查约束。特殊路径和热插拔传播需目标机验收，不能弱化 UF2 检查。bundle 不含运行时，首次安装可能下载，不是离线单文件；当前未申请 Flathub 上架。[权限](https://docs.flatpak.org/en/latest/sandbox-permissions.html)与 [bundle 限制](https://docs.flatpak.org/en/latest/single-file-bundles.html)以官方文档为准。

SDK 缓存默认 `${XDG_CACHE_HOME:-$HOME/.cache}/slimenrf-helper/flatpak/<架构>`，可用 `SLIMENRF_FLATPAK_CACHE` 改位置，`SLIMENRF_FLATPAK_JOBS` 改并行（默认 2）。不与原生缓存混用。磁盘不足优先换宽裕 Linux 文件系统；OSTree 的保留空间可使 df 仍有空间时拒绝导出，不自动降保护或清别人的缓存。WSLg 排查单次 X11 可用 `flatpak run --nosocket=wayland --socket=x11 io.github.jitingcn.SlimeNRFHelper`，不保存全局 override。

## CI 与发布门槛

[Native no-firmware CI](../.github/workflows/ci.yml)在 push、PR 和手动触发时运行。`ubuntu-24.04` 源码作业运行 Rust 工具测试（包括预发布准备测试），并检查公开边界和冻结的版权输入。通过后，`windows-2022` 与 `ubuntu-22.04` x64 作业独立运行。Windows 还运行工具测试以覆盖原生 hook；两者打包前都运行维护的质量入口。

源码和预发布作业使用相同的显式 Rust 缓存 key，沿用已有源码作业的前缀，不再分别追加作业 ID。编译器、环境和依赖哈希仍然启用。GitHub 的缓存作用域限制仍然生效：标签构建可以恢复默认分支的缓存，但相同 key 不会让其他标签的缓存全局可见。Windows 与 Linux 应用构建继续使用独立的缓存 key。

Windows 以普通文件名上传一个 UPX 压缩的 EXE。Linux 上传 AppImage。两个文件均不含固件，文件名包含 Cargo 版本，并作为 Actions artifact 保留 14 天，受仓库策略限制。CI 不构建 Flatpak，也不把缓存作为发布附件上传。

推送 `v<版本号>` 标签后，还会在三个前置作业全部成功后运行预发布作业。标签必须匹配主应用的 Cargo 版本和英文 `CHANGELOG.md` 中带日期的版本条目。发布说明使用该英文条目，并链接到同一提交的[中文更改日志](../CHANGELOG.zh-CN.md)。

发布作业下载本次运行的准确产物，不重新编译。它检查远程标签对应的提交，并拒绝已有的同标签 Release，包括草稿。固定到提交 SHA 的 `softprops/action-gh-release` 创建草稿并上传文件，禁止覆盖附件。最后一步核对两个附件的完整名称和大小集合，再公开预发布版本，不将其标记为 latest。上传或验证失败可能留下草稿。重试前先人工检查该草稿。工作流不会覆盖或删除它。

仓库权限保持只读，仅预发布作业拥有 `contents: write`。发布使用工作流令牌，不需要额外 secret。工作流不推送提交或标签。

### 发布预发布版本

1. 更新 workspace 版本、锁文件、冻结版权输入的哈希和两份更改日志。使用 UTC 发布日期。
2. 运行本次发布变更所需的质量与打包检查。
3. 在 `main` 提交发布变更。
4. 创建与 Cargo 版本一致的附注标签。
5. 一并推送 `main` 和标签。

以 `0.3.2` 为例，最后执行：

```bash
	git tag -a v0.3.2 -m "SlimeNRF Helper 0.3.2"
	git push --atomic origin main v0.3.2
```

等待标签对应的工作流完成。在 [Releases](https://github.com/jitingcn/SlimeNRF-Helper/releases) 核对预发布说明和两个下载文件。不要移动已发布的标签或替换附件。

CI 成功只证明配置的检查与产物上传已完成。本地构建、WSLg 渲染、原生 GUI、USB 权限和真实设备更新是不同证据；目标发行版、macOS、签名、性能及物理硬件仍须独立验收。应用自有材料的 MIT OR Apache-2.0 许可不能替代[上文](#license-notices)所述第三方材料、源码提供、固件和产物的独立审查，不由打包成功解决。

<a id="clean-and-troubleshoot"></a>
## 清理与排查

`./build.ps1 -Action clean` 和 `bash ./build.sh clean` 清理所选 target 的全部 profile 与嵌套缓存；Debug/Release 不缩小范围。可分别传 `-CargoArguments '--dry-run'` 或 `--cargo-arguments --dry-run` 预览，或显式 `--target-dir`。不删除 dist、配置、固件、全局 Cargo 下载或 Rust 工具链；先退出自己从 target 启动的程序，不例行 clean。

Fast 绘制或滚动排查可仅在单次启动设置 `GPUI_VIEW_RETENTION=0` 或 `GPUI_SCROLL_LAYERS=0`，结束清除；它们是诊断开关，不切换 GUI 后端，也不作为默认配置。软件 Vulkan 仅作兼容排查，不保证性能。macOS 需按上游安装说明配置本机依赖并单独验证；Linux package 入口不生成 macOS 包。

## 上游 bootloader fixture

从 [0.11.0-slimenrf](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/tag/0.11.0-slimenrf) 和 [0.11.1-slimenrf](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/tag/0.11.1-slimenrf) 获取以下资产放到 `.local/updater-fixtures`，不放开发固件目录：

```text
update-slimenrf_promicro_bootloader-0.11.0-slimenrf_nosd.uf2
update-slimenrf_promicro_bootloader-0.11.1-slimenrf_nosd.uf2
update-mochaelec_mochi_bootloader-0.11.1-slimenrf_nosd.uf2
```

```powershell
$env:SLIMENRF_UPDATER_FIXTURES = (Resolve-Path '.local/updater-fixtures').Path
./build.ps1 -Action test -NoFirmware -CargoArguments @('-p', 'slimenrf-helper', 'upstream_nosd_updater_payloads', '--', '--ignored')
```

```bash
SLIMENRF_UPDATER_FIXTURES="$PWD/.local/updater-fixtures" cargo test --locked --no-default-features -p slimenrf-helper upstream_nosd_updater_payloads -- --ignored
```

测试核对固定上游 SHA、UF2 内容与正确或错误 MCU/用途准入，不联网、不写设备，不代替物理更新。

<a id="firmware-oracle"></a>
## 独立固件 oracle

来源与数值边界见[协议说明](PROTOCOLS.zh-CN.md#numerics)。普通 `cargo test --locked -p slimenrf-recording` 使用提交的冻结外部检查点，不依赖开发者 checkout/WSL/SDK。完整逐事件测试默认忽略，先在有 Rust 工具链和 GCC 的 Linux/WSL 用真实固件（含对应 vqf-c）与完整 CMSIS SDK 生成 trace：

```bash
mkdir -p .local
cargo run --locked -p slimenrf-tools -- reference-oracle \
  --firmware /path/to/SlimeVR-Tracker-nRF \
  --cmsis-dsp /path/to/sdk/modules/lib/cmsis-dsp \
  --output .local/replay-oracle.csv
export SLIME_REPLAY_ORACLE="$(pwd)/.local/replay-oracle.csv"
cargo test --locked -p slimenrf-recording replay::tests::firmware_reference_trace -- --ignored --nocapture
```

生成器提取实际 wrapper/温度 MLS，链接外部引擎与 SDK 标量源，不链接被测 `replay_bridge.c`；同时输出 provenance 源码/trace 哈希与逻辑根参数，不发布本机绝对来源目录。省略 `--cmsis-dsp` 可生成 portable 基线，`--compare baseline.csv` 对照指定 trace。外部 SDK 需要的历史 C 编译器宏 `__GNUC_PYTHON__` 不是 Python 解释器依赖。

WSL 生成、Windows 测试必须访问同一文件；可用 `wslpath -w "$(realpath .local/replay-oracle.csv)"` 转换，不能用另一 checkout 的同名文件。配置好 Windows 原生环境后：

```powershell
$env:SLIME_REPLAY_ORACLE = (Resolve-Path ./.local/replay-oracle.csv).Path
cargo test --locked -p slimenrf-recording replay::tests::firmware_reference_trace -- --ignored --nocapture
```

这是受控主机算法对照，不是 ARM 逐位等价或在线状态重现证明，不因新输入漂移而重录历史期望值。
