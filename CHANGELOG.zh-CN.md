# 更新日志

[English](CHANGELOG.md)

本文件记录 SlimeNRF Helper 各版本的显著变更。

格式遵循 [Keep a Changelog 1.1.0](https://keepachangelog.com/zh-CN/1.1.0/)。
版本号遵循[语义化版本规范](https://semver.org/lang/zh-CN/spec/v2.0.0.html)。

## [未发布]

### 变更

- 根目录改为虚拟 Cargo workspace，桌面应用源码和构建支持移至 `crates/app`，与已有记录内核并列。根目录开发命令及资源路径语义不变。
- 开发测试脚本使用 cargo-nextest，文档测试作为单独操作运行；质量检查脚本运行两者。Windows MSVC 构建使用 Rust 工具链自带的 LLD 链接器，Linux 构建可选择启用 mold。
- Release 构建使用优化级别 2、fat LTO、单个 codegen unit，并剥离符号。
- 主窗口、版权说明、RSSI 分析和温度校准分析窗口改用随主题变化的 GPUI Kit 标题栏，不再使用系统标题栏样式，并保留窗口控制与退出清理流程。
- 常规 Python 命令不可用时，构建与 Linux 打包可通过 `uv` 查找已安装的 Python 3.11+，不会自动下载 Python 或创建环境。
- Windows 构建环境重复初始化时不再累积静态 CRT 参数或 Visual Studio 工具与编译器搜索路径，避免嵌套开发终端造成不必要的 Cargo 缓存失效。
- 版权材料需要审查时，debug 构建、运行和测试仍可继续；Cargo 会发出警告，版权页面显示待审查提示而非过期清单。Release 构建和打包仍要求使用最新且已审查的版权材料。
- Windows 打包和 CI 仅以普通文件名发布 UPX 压缩 EXE，不再提供单独的未压缩下载或添加 `-upx` 后缀。压缩改用 `--best --lzma`，不再进行耗时的 `--ultra-brute` 穷举搜索。

### 修复

- 追踪器卡片在非整数显示缩放下保持等宽，活动状态变化时卡片内容不再偏移。
- 在窗口可用高度较小或界面文字放大时，设置导航仍可滚动，并可通过键盘访问。
- 设置及其他可滚动页面的滚动条轨道与点击区域固定在各自视口内，滑块位置和拖动范围不再随内容滚动而偏移。

## [0.1.1] - 2026-10-26

### 新增

- 在**设置**的**关于**分组中新增 SlimeNRF Helper、接收器固件、追踪器固件和引导程序的项目仓库链接。
- 新增 CI 打包与预发布自动化，分别提供 Windows x64 普通 EXE、UPX 压缩 EXE 和 Linux x64 AppImage，均不含随包固件。

### 变更

- 版权材料改为在应用编译与打包暂存时生成，不再将生成文档纳入版本控制。应用构建、检查、测试、编辑器分析与打包现需 Python 3.11+；运行应用和只构建记录内核不需要。

### 修复

- 切换深浅主题后，终端已有文本的默认颜色与基础 ANSI 颜色随主题更新。扩展索引色和 RGB 颜色保持原有值。
- Windows UPX 打包不再传入不受支持的 `--exact` 选项，保留最高压缩设置。

## [0.1.0]

首个独立应用版本，根据仓库初始源码快照补充记录。

### 新增

- 接收器与追踪器发现、独立串口会话、多标签终端、命令历史、日志导出，以及危险设备命令的确认流程。
- UF2、OpenDFU、经接收器执行的 ESB OTA 和高级选项中的旧式 BLE DFU 更新，支持本地、随包及受支持的远程固件来源。
- HID 诊断、原始传感器数据采集、离线 VQF 分析、同步曲线和姿态预览。
- 陀螺仪校准指引、支持 SVG 导出的温度校准分析，以及 RSSI 信道分析。
- 英文与简体中文界面、深浅主题，以及支持完整固件和无固件构建模式的 Windows 与 Linux 打包。

[未发布]: https://github.com/jitingcn/SlimeNRF-Helper/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v0.1.1
[0.1.0]: https://github.com/jitingcn/SlimeNRF-Helper/tree/c1534d42a08b71e2bf9acdf0c33892766493080e
