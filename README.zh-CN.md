# SlimeNRF Helper

[English](README.md)

面向 SlimeNRF 接收器与追踪器的桌面应用，用于设备管理、串口终端、固件更新，以及传感器数据采集与分析。

## 功能

- 默认首页提供三步连接指引、实时设备状态、连接帮助，以及设备工具与固件更新入口。
- 接收器分组与追踪器发现，多串口独立会话、标签终端、命令历史和日志快照导出。
- 按设备角色提供中英文命令说明、参数输入、多目标预览与危险操作确认。
- UF2 更新、使用 OpenDFU 的 Holyiot 更新，以及通过接收器 HID 进行的 ESB OTA。高级设置中可启用 Legacy BLE DFU。
- 本地固件文件、完整构建中的随包固件，以及受支持的远程 Release 和 CI 产物。
- HID 数据与事件诊断、原始报文录制、原始传感器数据采集，以及带同步图表和姿态预览的离线 VQF 分析。
- 单一**陀螺仪校准**入口提供[静止校准与逐轴灵敏度引导](docs/USER_GUIDE.zh-CN.md#gyro-calibration)，面向 USB 直连追踪器或接收器下的在线追踪器。
- 温度校准曲线、拟合诊断、SVG 导出与 RSSI 信道图，支持串口结果展示和已保存日志导入。
- 英文与简体中文、浅色与深色主题，以及主题色设置。
- 所有平台支持[检查 GitHub Release 并打开发布页](docs/USER_GUIDE.zh-CN.md#app-updates)；官方 Windows 便携版还可下载校验，并由用户确认重启安装。

在**首页**选择**连接设备**，逐步检查接收器与追踪器。各步骤可自由浏览，也可从侧栏直接打开已有功能。指引展示实际观察到的设备活动，不将其视为设置完成。详见[首次使用](docs/USER_GUIDE.zh-CN.md#first-use)。

## 下载与平台限制

从 [Releases](https://github.com/jitingcn/SlimeNRF-Helper/releases) 下载 Windows x64 EXE 或 Linux x64 AppImage。预发布版本标记为 **Pre-release**，供测试使用。开发构建位于 **Actions** 中成功的 **Native no-firmware CI** 运行记录下的 **Artifacts** 区域。

- Windows EXE 文件名以 `windows-x64-no-firmware.exe` 结尾，使用 UPX 压缩。压缩可能引发安全软件误报或兼容性问题；不发布未压缩的备用版本。本地 Cargo 构建仍保留未压缩的原始可执行文件。
- Linux AppImage 以 Ubuntu 22.04 和 glibc 2.35 为构建基线，并非完全静态包。运行仍需兼容的宿主图形栈、字体、桌面与设备服务，以及 USB、HID 和串口权限。
- macOS 尚未单独验收。

CI 包内置默认配置，但不包含固件载荷。完整构建包含随包固件。无固件构建可使用本地文件、远程下载或显式选择的外部配置。

[安装与启动](docs/USER_GUIDE.zh-CN.md#installation)包含 AppImage 启动命令和无 FUSE 时的运行方式。[Linux 设备要求](docs/USER_GUIDE.zh-CN.md#linux-devices)说明权限与 UF2 挂载条件。[开发与打包](docs/DEVELOPMENT.zh-CN.md)说明源码构建方法。

## 固件更新安全

更新必须明确预览并确认目标、板型、角色、固件来源与风险。更新需要适配的固件、稳定供电和 USB 恢复路径。取消不会回滚已写入的数据或已发生的激活。传输完成也不能证明新固件运行正常。

[用户指南](docs/USER_GUIDE.zh-CN.md#firmware-updates)说明各更新方式、恢复限制，以及重新连接后的版本核对。

## 许可证

应用自有代码与资源采用 [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE)，由你选择。依赖、vendor 代码、第三方资源及外部固件保留各自条款。应用在编译时使用共享 Rust 版权材料库生成并嵌入 `THIRD_PARTY_NOTICES.txt`，生成文件不纳入版本控制。在**设置 > 查看许可证与声明**中离线阅读版权材料与源码获取说明，或参阅[版权材料维护流程](docs/DEVELOPMENT.zh-CN.md#license-notices)。

## 文档

- [首次使用](docs/USER_GUIDE.zh-CN.md#first-use)与[用户指南](docs/USER_GUIDE.zh-CN.md)：设备、终端、更新、采集与回放。
- [开发与打包](docs/DEVELOPMENT.zh-CN.md)：依赖、源码构建、检查、配置和分发。
- [架构](docs/ARCHITECTURE.zh-CN.md)与[协议及来源](docs/PROTOCOLS.zh-CN.md)：模块边界、格式、身份检查与数值限制。
- [更新日志](CHANGELOG.zh-CN.md)列出各版本的显著变更。
