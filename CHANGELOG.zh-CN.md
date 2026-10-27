# 更新日志

[English](CHANGELOG.md)

本文件记录 SlimeNRF Helper 各版本的显著变更。

格式遵循 [Keep a Changelog 1.1.0](https://keepachangelog.com/zh-CN/1.1.0/)。
版本号遵循[语义化版本规范](https://semver.org/lang/zh-CN/spec/v2.0.0.html)。

## [未发布]

### 变更

- 主窗口、版权说明、RSSI 分析和温度校准分析窗口改用随主题变化的 GPUI Kit 标题栏，不再使用系统标题栏样式，并保留窗口控制与退出清理流程。
- 常规 Python 命令不可用时，构建与 Linux 打包可通过 `uv` 查找已安装的 Python 3.11+，不会自动下载 Python 或创建环境。

### 修复

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

[未发布]: https://github.com/jitingcn/SlimeNRF-Helper/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/jitingcn/SlimeNRF-Helper/releases/tag/v0.1.1
