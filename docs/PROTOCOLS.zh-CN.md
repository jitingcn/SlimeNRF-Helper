# 协议、身份与数据来源

[English](PROTOCOLS.md)

[首页](../README.zh-CN.md) · [用户操作](USER_GUIDE.zh-CN.md) · [开发验证](DEVELOPMENT.zh-CN.md)

固定源码是协议解释与差分对照的依据，不证明用户设备当前安装的固件支持该能力。配置映射、产物存在、传输能力与实际硬件适配是不同条件。

<a id="board-product-sources"></a>
## 板型与产品来源

- Tracker 产品矩阵：[69190cb3](https://github.com/jitingcn/SlimeVR-Tracker-nRF/blob/69190cb39e7117c9116abf336a3f3bd4b487773b/.github/workflows/workflow.yml)。
- Receiver 产品矩阵：[674f50d2](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/674f50d27baeedc0438f340cda3fa99643ef6b3c/.github/workflows/workflow.yml)。
- 对照正式发布：[260910](https://github.com/jitingcn/SlimeVR-Tracker-nRF/releases/tag/260910)，接收器文件附在 Tracker 发布中，不把 Tracker tag SHA 当作接收器二进制来源 SHA。
- UF2 Board-ID、卷标、bootloader prefix 与 MCU：已核对 [Bootloader master 7f0654b8](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/tree/7f0654b898e92dd13f89c4fef108559a3d1c4b0e/src/boards)。已发布 updater 和 nosd 链接布局基线仍为 [b43e929b](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/tree/b43e929b1126fb7dc132a8453dc9593fa73765d8/src/boards) / [0.11.1-slimenrf](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/releases/tag/0.11.1-slimenrf)。
- nice!nano → SlimeNRF ProMicro 转换：[板级兼容性提交 b6c88ead](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/commit/b6c88ead6aa5b436062b3f6453f91b69020069d6)，不是全局 alias 或所有 clone 的保证。

矩阵核对基线为 2026-10-24，当前映射以 [YAML](../slimenrf-helper.yaml) 为准。Miro、P00、P10、RECV33、DawnRX-2、FoxSnackLiteV3、FoxDongleNEO 在该 CI 基线有产物但不在所查 260910 资产中。FoxDongleNEO 广告包含 Standard 和 DataCollect CDC；配置默认项为 Standard，不编造本地 CDC 默认资源。nRF54 六项只有 HEX；nRF54L15 的 Sense/ExtAnt 四组合共用 target 不能证明可互刷。矩阵无 MCUboot/DataCollect HID 产品，不构造默认下载；NekoNya receiver 只有 Standard。Holyiot 不编造引导，Aero Pro 不借用 Aero Lite。Kawazu 在后续 [master d1afd3e4](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/commit/d1afd3e4f9e85da4c16e6dd8c4023c9574508c0b) 才新增。所查 0.11.1-slimenrf Release 无 Kawazu 和两款新 Fox 板的 updater。引导程序 CI 是独立来源，提供仍有可用产物的成功构建。

FoxSnackLiteV3 固件使用 `foxsnacklitev3_uf2/nrf52840`，对应 bootloader 名称则为 FoxSnack V3。两者关联是**[INFERENCE]，由硬件源码交叉关联的推断**，不是从文件名推导：[tracker DTS](https://github.com/jitingcn/SlimeVR-Tracker-nRF/blob/69190cb39e7117c9116abf336a3f3bd4b487773b/boards/foxapplication/foxsnacklitev3/foxsnacklitev3_uf2.dts) 与 [bootloader header](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/blob/7f0654b898e92dd13f89c4fef108559a3d1c4b0e/src/boards/foxsnackv3_nrf52840/board.h) 的共阴 RGB 引脚 P0.12/P0.08/P1.09、功能按键 P1.06 一致。精确 Board-ID `FoxSnack-v3` 来自 header，不编造 `FoxSnackLite-v3`。[FoxDongleNEO header](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/blob/7f0654b898e92dd13f89c4fef108559a3d1c4b0e/src/boards/foxdongleneo_nrf52840/board.h) 提供 `FoxDongleNEO-v1`，对应接收器 target `foxdongleneo_uf2/nrf52840`。两款 bootloader 定义都声明 nRF52840、共享 `FOX-BOOT`；卷标仅为发现线索。两项映射均未经实物验证，也不复用旧 FoxSnackLite V2、FoxDongle33 或 FoxDongle 身份。

`board_targets` 新增身份来自精确 UF2 Board-ID，不用 Zephyr target 或产品名代替；共享卷标不能唯一认板，冲突或未知已报告 ID 不回退到卷标预设。`application_targets` 仅是产品、角色与文件映射，不参与 UF2 自动认卷或授予 OTA；缺少当前应用产品证据的板不编造应用文件。

## 资源与写入授权

文件、内置成员、下载资产与解包成员保留独立身份和 SHA；同 basename 不能混用参数或授权。精确默认资源不因缺失而选其他版本，显式 aliases 保留芯片、接线、Sense、角色、CDC/HID 与 MCUboot 区别。远程来源分离 application 与 bootloader；兼容筛选、目录排序、文件名或摘要一致都不能代替适配检查。

配置中的默认文件名还可识别同名的 UF2、HEX 编码和 DataCollect 类别。这些提示不创建下载项，也不授予写入权限。文件能否使用仍由更新方式和设备角色检查决定。

应用 CI 压缩包名称可以省略镜像扩展名。远程选择器为完整名称分别附加 `.uf2` 和 `.hex`，再按现有目录规则匹配，保留精确板型配置、角色和变种。例如，`SlimeNRF_ProMicro_Chrysalis_Tracker` 对应 `promicro_uf2/nrf52840/chrysalis`，不对应其他 ProMicro 配置。Release 资产和解包成员按各自的实际文件名匹配。压缩包名称不为成员提供板型身份或写入授权。

引导程序发行版和 CI 在 `remote_firmware.bootloader` 下分别配置仓库。引导程序 CI 只接受 `master` 分支中 [`.github/workflows/githubci.yml`](https://github.com/jitingcn/Adafruit_nRF52_Bootloader/blob/7f0654b898e92dd13f89c4fef108559a3d1c4b0e/.github/workflows/githubci.yml) 已完成且成功的 `push` 和 `workflow_dispatch` 构建，不使用应用固件的构建编号门槛。过期产物不列出。压缩包名称与配置的 `bootloader_board` 精确匹配，仅用于筛选。解包只提供 `update-*_bootloader-*_nosd.uf2` 成员，对被排除的成员也保留压缩包大小和路径检查。每个 updater 保留独立的文件名、摘要和板型分类。完整 HEX 和 DFU ZIP 不作为引导程序自更新文件提供。

应用写入与 bootloader 写入冻结不同用途。Adafruit nosd updater family `0xd663823c` 同时用于 nRF52833/52840，不是应用 family，也不标识 MCU。配置 `bootloader_mcu` 来自板级证据；写入冻结用途、配置 MCU、卷身份，核对 b43e929b nosd 链接布局及 CF2 flash/RAM/SoC family：bootstart=`flash−0xc000`、CF2=`flash−0x2800`、MBRparams=`flash−0x2000`，仅允许相应 MBR/bootloader/UICR 范围，拒绝 SoftDevice、应用与错芯片镜像。不承诺未来或自定义布局兼容，也不向通用应用白名单加入该 family。

Linux UF2 只接纳真实可写 FAT 块设备与 UF2 标记，卷标按 `/dev/disk/by-label` 对应块设备号获取，不把目录名当卷标。写前复核挂载实例与 device identity；同路径换盘、重挂载或 mount incarnation 变化使旧授权失效。全局 mountinfo 可以包含合法伪文件系统 root，只有 UF2 候选才要求原生块设备与绝对路径条件；不能让无关 namespace 挂载破坏全部枚举。

UF2 复制区分主机 I/O 已完成和 `CopyOutcome::ConfirmationPending`。只有全部字节已被接受、取消检查已通过，且 `flush` 或 `sync_all` 返回设备消失错误时，才进入待确认状态。接受的错误码为 Windows 433、1167，以及 Linux 和 macOS 的 `ENODEV`、`ENXIO`。部分写入失败、零字节写入和其他存储错误仍判定为失败。`UpdateOutcome::Uf2CopyUnconfirmed` 保留已接受字节数、收尾阶段、系统错误码和诊断。复制完成和复制待确认都不证明固件已启动。

OpenDFU 根据原生 USB 接口描述识别引导程序，仅在该描述缺失或不可用时回退到原始 USB 产品名称。显示标签、制造商名称和 VID/PID 本身不构成引导程序身份。有歧义或冲突的接口描述会拒绝识别。每次打开原生串口前，包含接口元数据的串口枚举会复核已冻结的端口与 USB 身份。端口消失、重复别名、枚举失败或身份替换会停止操作。更换 COM 口须显式选择。

<a id="esb-ota"></a>
## ESB OTA 与 HID 命令

协议基线：[Web OTA 51bf6e01](https://github.com/jitingcn/slimenrf-ota-web/tree/51bf6e01d550ceb537bcee0f1111780c100f5bd1)、[Tracker 5daf6f39](https://github.com/jitingcn/SlimeVR-Tracker-nRF/tree/5daf6f39ca6f53f19c9abd7455d3db2d774cc040)、[Receiver 9ee5e788](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/tree/9ee5e788346018a0b390bdacec372cc6f7ea7000)。此 Tracker 是早期协议参考，不是回放参数基线。

- Web 实际使用 [app.js 串行队列](https://github.com/jitingcn/slimenrf-ota-web/blob/51bf6e01d550ceb537bcee0f1111780c100f5bd1/js/app.js#L617-L638)逐次调用 queryInfo，不是未调用的 batch helper。每接收器单在途请求，逐目标从写入完成独立计时 3 秒，初始稳定等待每会话一次，无自动重试或逐目标固定间隔。
- 前五块足够校验载荷，但第六空尾块才是正常中继边界；CRC 成功或错误都先等尾块。缺尾只等原截止，保留有效且身份匹配的元数据再推进，不延长或重发。
- [接收器共享回复缓冲](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/9ee5e788346018a0b390bdacec372cc6f7ea7000/src/esb_ota.c#L121-L161)与缺少事务 token 意味着迟到 RF 回复仍可能重叠后续请求。串行降低风险，但不能消除风险。助手的重试排空也无法绝对区分排空结束后才到达的旧回复。缺失回复、CRC 失败和身份不匹配均如实报告。
- 页面缓存绑定接收器路径与 serial、slot/RF 地址，成功时间按单调时钟计 30 秒；不作为执行授权。刷写在独占会话重新读取地址与完整 FW_INFO，任何已观察到的地址变化，包括 A→B→A，均拒绝旧绑定。缺当前会话身份证据仍拒绝。
- 助手的普通页面与发现查询仍只尝试一次，每个接收器只有一个在途查询。每次实际查询均从写入完成后独立计时，响应收集预算为 3 秒。
- 执行前检查只在 `FW_INFO` 仍不完整时允许额外尝试一次。追踪器查询要求当前会话已正向确认匹配的 RF 身份。接收器本地查询使用已准入的接收器会话，同样允许这次重试，不要求追踪器 RF 证据。
- 额外尝试前，助手在保留身份检查的同时进行可取消的 500 ms 接收排空，丢弃迟到信息，再创建全新的分块组装器。CRC 失败、载荷格式错误、缺少追踪器 RF 证据、身份不匹配或槽位重分配、完整元数据变化、传输失败及取消均不触发重试。载荷完整有效而仅缺空尾块时，仍按上文保留原截止时间与元数据处理方式。
- 同接收器所有追踪器镜像组在第一次 BEGIN 前完成新鲜检查，跨批次保护尚未执行目标，接收器自更新最后重新校验；不在前批激活恢复窗口插入后组查询。
- 并发取决于追踪器 SoC。nRF52840 最多同时更新两个追踪器，其他追踪器逐个更新，接收器更新独立进行。尝试 BEGIN 前失败绝不发送 ABORT。有效完整的 COMPLETE 后不发送 ABORT。尝试 BEGIN 后，追踪器失败、取消或未完成时，仍最多尝试一次定向中止，并按精确接口身份保留已知 10 秒准入保护。后续无线查询与 BEGIN 等待该保护期，等待可取消。接收器本地查询不等待无线保护期。
- 接收器在尝试 BEGIN 后、尝试 ACTIVATE 前失败或取消，仍执行原有清理。助手在尝试 ACTIVATE 写入前立即撤销该接收器的 ABORT 资格。部分写入或写入报错仍可能已送达固件。尝试后不发送 ABORT、不重试 ACTIVATE、不自动重发激活，也不声称回滚。此规则不改变追踪器清理，也不在 worker 完成 join 前释放所有权。
- VERIFY_OK 与 COMPLETE 必须匹配当前镜像的包数和字节数。接收器 ACTIVATING 与 COMPLETE 还必须匹配接收器目标 ID。有效 COMPLETE 仍表示协议成功，包括固件信息查询期间迟到的 COMPLETE。COMPLETE 表示激活交接，不证明新固件已启动。单独的 ACTIVATING 不证明完成。明确的激活失败、身份矛盾、匹配但格式无效的状态及不完整的完成声明仍属于失败。
- 原生接收器重开接受第一个与冻结 HID 路径、USB 厂商 ID、产品 ID、usage page 及 serial 匹配的新句柄，不要求先打开失败或先观察到断开。句柄重开不是启动证明。新鲜且兼容的 FW_INFO 仅证明接收器有响应，不证明激活完成或目标镜像已启动。
- 接收器激活观察共用一个绝对 30 秒预算，从 ACTIVATE 写入尝试前开始。状态等待、原生重开及固件信息查询等待共用该截止时间。每次等待均受剩余时间限制。重开或重试暂时不可用状态不重置时钟。身份矛盾与无效证据不重试。
- 没有有效 COMPLETE 时，激活可终结为 `UpdateOutcome::ActivationUnconfirmed(ReceiverActivationReport)`。报告将 `ActivationSubmission` 的 `Sent`、`DeliveryUncertain` 或 `Accepted` 提交证据，与 `ReceiverAvailability` 的 `Responding`、`Unconfirmed` 或 `Detected { prior_diagnostic }` 可用性分开。UI 显示终态**待核实**结果，不表示成功、失败、取消或仍在活动的进度。写入尝试后取消不会把不确定的送达情况改为已取消更新。缺少事务 token 及最终 COMPLETE 可能丢失仍是协议边界。
- worker 完成 join 后，UI 可根据现有共享清单，将 `Unconfirmed` 可用性改为 `Detected { prior_diagnostic }`。扫描的开始时间必须严格晚于 join 完成后的记录时间，并通过有效 USB 序列号唯一识别已冻结的接收器物理身份键。COM 编号、HID 路径和追踪器槽位不构成该身份，因此 HID 路径改变不妨碍匹配。此协调不启动固件查询或读取者。先前的可用性诊断保留在**激活期间的观察**中，不改变 `ActivationSubmission`，不将 `UpdateOutcome` 或 `OtaPhase` 升级为完成，也不证明已安装的镜像或版本。协调仅适用于保留的未决接收器结果；全部检测到、开始新更新流程或操作、关闭操作结果后，协调资格清除。

接收器 CDC 分类要求 USB 厂商 ID `0x1209`、产品 ID `0x7690`，以及去除首尾空白后精确匹配的原生接口描述 `SlimeNRF Raw Data CDC`。产品名或显示标签不能代替该描述。共享清单将 raw 数据接口与上下文控制台关联到一个唯一识别的 HID 接收器。唯一剩余的非 raw 候选无需接口名称也可作为控制台。重复或歧义证据拒绝关联，多个剩余候选不提供控制台。命令使用控制台准入，CDC 采集则仅准入显式 raw 数据接口。

通用 HID 命令能力按 [rcv_hid_cmd.h](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/9ee5e788346018a0b390bdacec372cc6f7ea7000/src/rcv_hid_cmd.h) 与 [rcv_cmd.c](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/9ee5e788346018a0b390bdacec372cc6f7ea7000/src/rcv_cmd.c) 核实：type 254 命令、type 251 ACK，参数需二进制编码，远程串行且 STARTED 不等于最终 ACK；`all` 指活跃追踪器。info/list/uptime 等文本仍需 USB CDC。Tracker 的该版 hid.c 只有 INPUT 报告。现有 OTA 与采集专用 HID 控制不等于已交付通用 HID 命令面板；不能失败后自动跨路由重发。

事件依据 [events-watch c6cfb46d](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/c6cfb46df2d527052bfa8a4b92f9d1a7c4f7e478/scripts/hid_cmd.py)，保留 tracker/receiver observation、snapshot、boot/operation/event sequence、用户或自动来源、阶段、结果及原因。ACK 不冒充校准完成，未知字段保留原值。

<a id="gyro-calibration"></a>
## 陀螺仪校准证据

校准参考为 [Tracker 8aa61a6c](https://github.com/jitingcn/SlimeVR-Tracker-nRF/tree/8aa61a6ce0a9e3bc6f45564656f808827da5de01) 与 [Receiver aad8eae3](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/tree/aad8eae3ec8745bbc35c2ca638ac8468c7eed0bc)，不替换上方较早的 OTA 基线。操作步骤见[陀螺仪校准](USER_GUIDE.zh-CN.md#gyro-calibration)。

指引使用一个已识别的直连 USB 追踪器控制台，或通过接收器主 HID 操作一个在线追踪器。直连命令为 `calibrate` 和 `sens auto <axis> <revolutions>`。灵敏度指引显式发送传感器 X、Y 或 Z 轴及 1–100 圈。HID 使用 opcode `0x02` 加 `[slot]`，或 `0x24` 加 `[slot, axis, rev_lo, rev_hi]`，X、Y、Z 分别编码为 0、1、2。输入报告为 64 字节，包含四条 16 字节记录。输出含零报告 ID 前缀，共 65 字节。编码依据[接收器命令定义](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/aad8eae3ec8745bbc35c2ca638ac8468c7eed0bc/src/rcv_hid_cmd.h)。

结果分开保留以下证据。

|证据|含义与边界|
|---|---|
|写入回执与接收器 ACK|表示字节已写入或远程命令确认。`QUEUED`、`STARTED` 及接收器最终 `OK` 均不证明追踪器接受或应用。确认超时仍可能已送达。|
|追踪器接受或拒绝|新的匹配用户 `ACCEPTED` 事件绑定该操作。新的匹配追踪器 `REJECTED` 事件若带有 `FAILED` 状态，无需先有接受事件也可证明拒绝。接收器支持事件不证明追踪器支持遥测或灵敏度校准。|
|应用|已绑定追踪器的 `END` 事件带有 `SUCCESS` 状态和 `APPLIED` 标志，或受支持的精确直连灵敏度应用响应，证明已应用。普通直连静止校准的 `Calibration queued for next sensor frame` 只证明采集与排队。之后舍入的 `info` 观测不是应用回执。|
|存储|匹配的存储错误在应用前后都保留。已确认应用与持久化失败可同时成立。协议无正向存储成功回执，观察期间静默不能证明已保存。|

接收器订阅使用 opcode `224`、版本 `1`、能力位 `0`、精确槽位及校准掩码 `1`。`receiver_events::Watch` 校验返回版本、过滤槽位、激活标志与掩码。worker 在命令 ACK 等待、订阅续期和清理期间消费每条输入记录，不以有损诊断订阅作为权威来源。

关联包含冻结的接收器与原生打开身份、槽位和新鲜 RF 注册、事件会话 nonce、操作 ID、种类及用户来源。解码器将 nonce 命名为 `boot_id`，但其变化不唯一证明重启。worker 有界排空既有输入，并在一次操作写入前建立待关联状态。旧事件、自动事件与无关事件不会被采纳，孤立终态也不会绑定。环绕感知去重保留乱序事件；接受事件绑定后，可重放先到达但序号更晚的缓冲事件。同一操作中较旧的乱序存储错误仍然有效。载荷冲突、多个竞争接受、事件会话改变及观察到的 RF 替换，包括 A→B→A，均使归因不确定。命令没有主机请求 token，因此同时发生的同类外部操作或实体按键请求仍是归因边界。身份语义依据[追踪器事件协议](https://github.com/jitingcn/SlimeVR-Tracker-nRF/blob/8aa61a6ce0a9e3bc6f45564656f808827da5de01/src/connection/tracker_event_protocol.h)及[接收器事件处理](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/aad8eae3ec8745bbc35c2ca638ac8468c7eed0bc/src/tracker_events.c)。

接收器 `END/UNKNOWN` 表示观察丢失，不是追踪器取消。之后已绑定的追踪器终态可加强结果证据。接收器会去重相同的两秒心跳，主机缺少心跳不证明失败。助手从发送尝试起将执行观察限制为 180 秒。终态结果开始固定 20 秒观察尾窗；直连静止校准排队则开始独立的 20 秒最终观察窗，不证明已应用。重复事件不延长窗口。这些是主机策略，不是固件完成或存储截止时间。权威输入仍打开时，包括有界清理期间，结果可继续变化；读取者关闭后不再获得新证据。

直连准备要求完整新鲜的 `help` 和 `info` 响应，且追踪器身份与命令能力匹配。[追踪器控制台](https://github.com/jitingcn/SlimeVR-Tracker-nRF/blob/8aa61a6ce0a9e3bc6f45564656f808827da5de01/src/console.c#L817-L845)从当前 retained 数组打印五位小数的灵敏度系数。[bias 输出](https://github.com/jitingcn/SlimeVR-Tracker-nRF/blob/8aa61a6ce0a9e3bc6f45564656f808827da5de01/src/console.c#L995-L1007)来自 `sensor_calibration_snapshot()`，表示当前已应用 bias，不是单独存储的静止 bias。前后值只是受显示精度限制的观察，不是无损备份或持久化证明。比较可在显示精度下标出其他轴系数和所观察 bias 的变化；目标轴系数不变也是有效结果。无线系数比较不可用。raw `0x14` 与 `collectmeta` 不提供新鲜校准读回。

**停止观测**只结束主机观测，不取消设备校准。操作写入前停止不会发送校准命令。可能写入后，设备仍可能运行，之后的命令可能冲突或被拒绝。固定协议没有不修改设备状态的校准取消命令。重置系数、替换增益、重启、raw 采集 STOP 及 OTA abort 都不是取消替代品。租约保留到原生清理及 join 完成。join 后确认未决结果只允许继续操作，不改变设备证据。普通直连静止校准排队结果无需例行断电重启。

<a id="recording-formats"></a>
## 记录格式与 raw 语义

`.hidlog` 是 `slimenrf-hidlog` schema 1 UTF-8 JSONL：header 保存版本、UTC、单调时钟起点、微秒单位与快照标记；source 保存接收器、USB serial、原生路径字节、主/raw 接口、VID/PID/usage 与连接 ID；record 保存全局序号、单调时间、IN/OUT 原字节、报告 ID 前缀语义、实际写计数或错误及连接生命周期。gap/discontinuity 显式记录已知丢失或保留序号中断；end 保存排空后的数量统计，缺 end 不视作完整录制。日志可包含真实身份，分享前须脱敏。

`.slimecap` v1 是解析采样的追加 JSONL，保存身份、元数据、校准、样本与结束；中断可恢复完整记录但标明不完整。CSV/ZIP 使用配对 metadata。缺温度为缺失或 NaN，不填 0°C；两种文件不是互换格式，hidlog 也不是系统 USB 抓包或虚拟设备。

录制交换格式采用以下主机限制：

| 项目 | 上限 |
|---|---|
| 原始 CSV 记录，包括引号和字段内换行 | 16 KiB |
| 每份录制的轨迹数 | 512 |
| 整份录制的观测数 | 5,000,000 |
| 每个文件或 ZIP 成员的元数据 | 1 MiB |
| CSV 输入或导入的 ZIP 文件 | 1 GiB |
| ZIP 成员累计解压字节数，导入和导出均适用 | 1 GiB |
| ZIP 中央目录 | 4 MiB |
| ZIP 成员数 | 1024，可容纳 512 对 CSV 与元数据 |

加载器在构造 `ZipArchive` 前，使用有界结束记录和中央目录缓冲验证归档外层结构。分卷、ZIP64、超限或不一致的目录与成员计数、无效成员偏移、格式错误的额外字段，以及该目录与结束记录快照中相互竞争的结束记录均被拒绝。

成员校验拒绝不安全名称、目录、符号链接、重复名称，以及 `.csv` 和 `.meta.txt` 之外的扩展名。名称上限为 1024 字节。绝对路径、目录穿越、空路径分量或点分量、反斜杠、冒号和 NUL 字节均被拒绝。元数据必须有同名 CSV 配对，但 CSV 可以没有元数据。实际解压成员大小必须与声明一致，声明大小和实际内容均受导入上限约束。

CSV 与配对元数据 ZIP 导出在写入字节时检查累计解压大小。导出先写入并同步同目录临时文件，仅在成功后替换目标。取消或失败时，已有目标文件保持不变。

raw HID Usage Page `0xFF00` 的 `0x13` 与 CDC CRC 帧由专用解析器处理。`0x14` 携带 raw 采集会话开始时捕获的不可变校准元数据。`collectmeta` 只请求重传该快照，不获取校准后的新鲜系数，依据[追踪器采集会话快照](https://github.com/jitingcn/SlimeVR-Tracker-nRF/blob/8aa61a6ce0a9e3bc6f45564656f808827da5de01/src/connection/connection.c#L1044-L1088)。校准指引不为取得读回而启动或重启 raw 采集。控制与单批 ACK 走主 HID。raw wxyz float 累积未补偿陀螺积分四元数不是融合姿态；主 HID 融合四元数为 xyzw Q15、线加速度 m/s²，raw 加速度 g，不能混解释。

<a id="numerics"></a>
## 数值与坐标边界

回放基线：[Tracker 8aa61a6c](https://github.com/jitingcn/SlimeVR-Tracker-nRF/tree/8aa61a6ce0a9e3bc6f45564656f808827da5de01) 与 [vqf-c 3c1a6ae6](https://github.com/jitingcn/vqf-c/tree/3c1a6ae6e931c98b652723372e52d9ed4b7819e2)。实际字节、许可与构建约束见 [VQF provenance](../crates/recording/vendor/vqf/provenance.json) 和 [CMSIS provenance](../crates/recording/vendor/vqf/cmsis/provenance.json)；提交号不保证开发者工作树相同。

使用实际固件 C、标量 CMSIS 与安全 FFI，保留参数、float/double 混合精度、校准顺序和温度 MLS/cache。旧 CSV 只有完整 `gyro_tcal` 时可明确标为 `legacy_csv` 离线曲线，要求至少四完整点与可用温度，不推断设备 thermal flags/mode/LUT/运行 offset，不重分类现代禁用、部分或索引数据；导出保留来源，实际设备温度头优先。

时钟按 raw TX 速率和序号跨度，不以接收器到达或芯片 ODR 代替；保留 fresh accel/new-mag，未知时长中断分段冷启动。高于 TX 的 gyro rate 仅构造区间恒定角速度子事件，不能恢复未记录芯片观测。设备先前滤波状态、运行校准、自适应历史、隐藏丢样、磁时间戳及 ARM FPU 优化不可完整重建，不宣称与在线设备逐位一致。完整外部 oracle 操作见[开发文档](DEVELOPMENT.zh-CN.md#firmware-oracle)。

Auto A/B 要求单连续分段、`N×dt ≥ 60s`。A 选取首个在前 60 秒内包含至少 5 秒的 rest 区间，将区间末端截到 60 秒后，取该末端前 5 秒的参考点。B 选取整段最后一个持续至少 10 秒的完整 rest 区间，取其末端前 10 秒的参考点。两点按最近样本取整，半样本取较晚样本，并限制在各自原区间内。两点须来自不同原区间且 B>A，不替代搜索或部分应用。端点航向相对分段约 1 秒基准，区间为绕回带符号 B−A 与 °/min；同点无变化率，跨重置无连续漂移，非真值或精度认证。

姿态预览采用 Hamilton wxyz、body→world、Z-up，相机独立；本体 XYZ、世界重力、合格磁与 6D 线加速度向量由设备中心叠加，缺失隐藏，箭头为有界方向示意而非物理尺度。外壳是轻量装配示意，不是精确 CAD、点云或校准覆盖率工作区。

<a id="thermal-dump"></a>
## 温度 dump 分析

输入是固件控制台的人类可读 dump，保留稀疏 slot、带符号温度与 bias °/s，但打印值已舍入；不是 raw binary snapshot 或无损校准备份。头、尾与行数严格匹配，精确空响应可无终止行；未知、错误或不完整响应不能变成成功空曲线。主机上限 512 点、slot 0–511、单行 4096 原始字节、响应 256 KiB。

数学保留旧分析流程：全局线性拟合；最多十个近邻的加权 MLS，scale 2.5、cutoff 0.03、singular threshold `1e-10`；边界 4°C 窗口分别向两侧外推 6°C，不将负温度截到零。预计算内部 100 点、每侧 50 点；统计为残差平方和、有效相邻斜率范围与 bias 范围，不把平方和标成均方根。空或单点不拟合，重复温度使用定义的常数或加权均值，避免无效相邻除法。规范化文本若用于内部处理只保留导入数字与顺序，不恢复固件打印丢失精度。

读回只走既有已连接、角色确认的直连串口；不建立新设备通路，不控制加热或写回。协议无请求 nonce；已启动而未完成的读取取消、超时或早期解析失败后隔离该端口，须显式断开再重连，不能把新命令或迟到旧响应误配成下一次 dump。TCAL 专属空响应、完整成功或消费完整终止行后的计数错误不因此隔离。通用 `Command unavailable in this firmware` 也可能来自此前命令，在线收集头部前只保留为候选错误，仍等实际 dump 头、空响应或完整结果到原绝对截止时间，不按静默窗口提前完成；只有通用错误到截止时失败并隔离。离线文本导入则可立即报告 Unsupported。SVG 保存为当前分析与视窗快照，不是设备数据恢复文件。

<a id="rssi-scan"></a>
## RSSI 信道扫描

依据 [Receiver aad8eae3 的扫描实现](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/aad8eae3ec8745bbc35c2ca638ac8468c7eed0bc/src/connection/rssi_scan.c#L266-L418)和[信道集合](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/aad8eae3ec8745bbc35c2ca638ac8468c7eed0bc/src/connection/esb.c#L75-L91)。解析现代带符号 dBm 输出，保留头部范围、当前与推荐信道和明确失败排除的信道。每信道结果是扫描样本中最强的 RSSI，不是平均值或追踪器链路强度；推荐值直接来自固件，不由主机重新计算。信道按编号排序，图中每个收到的测量值在实际数字信道坐标处显示一根柱，柱顶对应测得的 dBm 值。报告范围内未采样或被排除的整数信道在基线附近显示中性、低对比度的小 × 标记，与**未采样**图例一致；它们表示没有可用样本，而非测得的零值或信号高度。实际信道位置与间隔保持不变，柱之间的普通空隙保持空白。悬停实测柱会立即显示信道及 dBm 读数，悬停缺失信道整列高度内的任意位置会立即显示信道及**未采样**。纵轴根据观测样本自动留出余量并取整，每根柱从当前最下方的标注刻度开始；新的实时观测可能使坐标轴和柱形重新缩放。dBm 是对数值，且基线自动缩放并截断，因此柱高不代表功率比。当前和推荐信道标记彼此独立。图中不硬编码当前固件的 41 项集合，也不为缺失信道虚构数值。

输出没有结束标记、返回条数或最终确认；TPS 日志、短行和静默均不能证明扫描完成。图窗接收变化后的实收快照，不增加等待超时、重试或推测的完成协议。串口分片和 ANSI 控制串不能变成伪测量；冲突数据、损坏的结果行及扫描错误会使当前解析失效，后续有效头部才能开始新扫描。

`RSSI scan already in progress` 保留正在接收的扫描；`RSSI scan busy: radio in use` 则是新请求在输出头部前被拒绝，不允许导入回退到先前结果。区别来自[共享异步扫描门控](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/aad8eae3ec8745bbc35c2ca638ac8468c7eed0bc/src/rcv_cmd.c#L46-L65)和[控制台响应](https://github.com/jitingcn/SlimeVR-Tracker-nRF-Receiver/blob/aad8eae3ec8745bbc35c2ca638ac8468c7eed0bc/src/console.c#L1075-L1082)。明确无有效测量是空结果，不是 0 dBm。

导入与在线观察共享解析规则：文件最多 256 KiB、单行最多 4096 原始字节，信道 ID 为 0–255，RSSI 为 −127–0 dBm；这些是解析上限，不代表设备支持全部信道。导入只取最后一个可识别扫描；新扫描缺数据或失败时不静默回退旧扫描。文件末尾可结束语法完整的一行，但不能据此声称固件完成。
