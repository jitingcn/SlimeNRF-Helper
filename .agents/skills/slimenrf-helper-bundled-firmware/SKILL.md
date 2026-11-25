---
name: slimenrf-helper-bundled-firmware
description: Package SlimeNRF Helper with selected Tracker/Receiver and Bootloader release tags. Use for 含固件打包, new board mappings, embedded firmware tag display, or verification of a full firmware bundle.
---

# SlimeNRF Helper 含固件打包

从仓库根目录操作。以用户指定的 Tracker（应用）和 Bootloader（引导）release tag 为准；固件 tag 不等于桌面应用版本。默认交付当前平台的本地构建，不打 Git tag、不上传发布、不操作实物。

## 1. 核对来源与板型

- 按 [slimenrf-helper.yaml](../../../slimenrf-helper.yaml) 的 `remote_firmware` 查询两个仓库的 GitHub Releases API `repos/{owner}/{repo}/releases/tags/{tag}`。核对返回的 tag、发布状态、源提交和资产清单，不用 `/latest` 或文件名猜版本。Tracker 发布可附带 Receiver 产物；Receiver 源提交从发布说明或对应成功构建单独核对。
- 对照固定提交的构建矩阵、板级源码和 bootloader `board.h` / `board.mk`。按实际证据核对应用目标、Tracker/Receiver 角色、接线或硬件变体、Standard/DataCollect CDC、UF2 Board-ID、MCU 和更新包前缀。`board_targets` 只使用真实 Board-ID；仅有应用目标证据时放 `application_targets`，不新增卷标回退。共享 Board-ID 或运行时 target 不能区分接线时，提供明确的有线选项，不自动分配 OTA。不把相近板型或同 MCU 当成同一设备。
- 仅修正所需映射、正式资产名及受影响断言；同步 [板型来源](../../../docs/PROTOCOLS.md#board-product-sources)、中文对应节和双语 changelog。应用固件与引导更新的发布覆盖范围可能不同。某板没有该 tag 的发布资产时，保留有证据的本地/CI 匹配，不从旧版或 CI 补入本次包，也不编造默认项。

## 2. 暂存精确版本

- 在 `.local/` 下建新的、按两个 tag 区分的暂存目录，把固件文件与清单分开。构建会收录固件根目录顶层的所有 `.uf2`、`.hex`、`.bin` 和 `.zip`，不只收录映射到板型的文件。只把当前软件能识别和使用的正式发布资产放进去；引导程序选与板型匹配的 `update-…_nosd.uf2`，不要混入 BOOTLOADER HEX、SoftDevice ZIP、同名旧文件或不受支持的芯片镜像。完整包不等于把整个 Release 复制进 EXE。
- 对每个入包资产记录 tag、源 URL、文件名、字节数和 GitHub 公布的 SHA-256。下载后按清单比较长度与 SHA-256，不匹配或缺少可信摘要的文件不入包。私有固件根只通过 `SLIMENRF_FIRMWARE_ROOT` 传入，不改 YAML 的可移植路径，不改外部开发固件目录。
- 用现有 `slimenrf-firmware-image` 的 `parse_image` 和 `validate_uf2_content` 离线验证入包镜像。引导更新必须以 `Uf2ImagePurpose::Bootloader(配置中的 MCU)` 通过，以另一 MCU 和应用用途被拒绝；两种 nRF52 共用的 updater family 不是 MCU 或板型证据。不为让某个产物通过而放宽检查。

## 3. 构建并核验成品

- 使用 [项目工具链和质量入口](../../../docs/DEVELOPMENT.md#checks-and-editor)。涉及映射、资源或构建规则时，两种固件模式均跑质量检查；无固件检查不设置 `SLIMENRF_FIRMWARE_TAG`。Release notices 必须通过，不能用开发模式绕过；对外分发前另行做[固件与产物许可审查](../../../docs/DEVELOPMENT.md#license-notices)。
- Windows：`$env:SLIMENRF_FIRMWARE_ROOT` 指向已核验的暂存目录，再运行 `./package.ps1 -FirmwareTag $trackerTag`，其中 `$trackerTag` 是实际应用固件 tag。不传 `-NoFirmware` 或 `-OfficialReleaseTag`。标题和「关于」显示 `<应用版本>+fw.<应用固件 tag>`，相同应用版本的 EXE 文件名不变；只在确定允许替换同名包时使用 `-Force`。其他平台按 [开发文档](../../../docs/DEVELOPMENT.md#linux) 的原生入口打包，本地含固件构建显式设置 `SLIMENRF_FIRMWARE_TAG` 和 `SLIMENRF_FIRMWARE_ROOT`。
- 核对最终产物，不只核对下载目录。用项目所固定的 UPX 从最终 Windows EXE 解压到独立临时文件，定位其中与当前 Release `OUT_DIR/firmware_bundle.zip` 字节一致的 ZIP。比较 ZIP 的完整成员集合（固件清单 + `slimenrf-helper.yaml`）、逐成员大小和 SHA-256；检查生成索引的缺失默认项及包内板型映射。打包成功不代表默认资源都存在，也不代表固件 tag 与字节吻合。
- 按 [原生 UI smoke](../../../docs/DEVELOPMENT.md#native-ui-smoke) 启动同一成品。可执行文件目录与工作目录均不得有覆盖用的 `slimenrf-helper.yaml`。查看标题和「关于」显示的 tag；验证 UI 可离线加载一个内置镜像、显示 `Bundled` 来源和清单中的摘要。Windows 使用自有 PID/HWND；加载镜像即可，不选择实体端口、不进入 DFU、不开始 OTA 或刷写。验证后正常退出并移除临时工具。

交付时列出产物路径、EXE 的 SHA-256、两类固件 tag 和数量、影响选择的板型差异与未包含的资产。分别报告离线验证、Windows 原生、其他平台和实物验证的实际结果。
