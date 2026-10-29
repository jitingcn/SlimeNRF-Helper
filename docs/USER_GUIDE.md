# User guide

[简体中文](USER_GUIDE.zh-CN.md)

[Project home](../README.md) · [Development and packaging](DEVELOPMENT.md) · [Protocols and sources](PROTOCOLS.md)

<a id="installation"></a>
## Install and start the application

1. Open the project's [Releases](https://github.com/jitingcn/SlimeNRF-Helper/releases) page.
2. Select a version. Choose a **Pre-release** if you want a test version.
3. Under **Assets**, download the package for your platform.

For a development build, open **Actions**, select a successful **Native no-firmware CI** run, and download its **Artifacts**.

On Windows, download the package whose filename ends in `windows-x64-no-firmware.exe` and run it directly. It is UPX-compressed, which can trigger security software or compatibility problems. No uncompressed fallback is published; a local Cargo build retains the original uncompressed executable.

On Linux, download the AppImage. Replace the example filename below with the actual filename. Run these commands in the directory that contains the file.

```bash
chmod +x ./slimenrf-helper-0.2.0-linux-x64-no-firmware.AppImage
./slimenrf-helper-0.2.0-linux-x64-no-firmware.AppImage
```

If FUSE is unavailable, start the application with this command:

```bash
./slimenrf-helper-0.2.0-linux-x64-no-firmware.AppImage --appimage-extract-and-run
```

The no-firmware package includes the default configuration but no firmware payloads. For updates, use local files, remote downloads, or an explicitly supplied external configuration. For Linux services, device permissions, and UF2 mount requirements, see [Linux device and runtime requirements](#linux-devices). For source builds, see [Development and packaging](DEVELOPMENT.md).

<a id="first-use"></a>
## Use the application for the first time

The application opens **Home** on each launch. Use **Language and appearance** to open Settings. If saved preferences cannot be read, the application opens Settings instead and leaves the existing file unchanged.

Choose **Connect your devices** for the connection guide:

1. In **Prepare your devices**, get a receiver and a USB data cable. Charge your trackers and keep them nearby. Working devices do not need a firmware update first.
2. In **Connect the receiver**, plug the receiver into the computer. Watch the device summary. If the receiver does not appear, try another data cable or USB port.
3. In **Check your trackers**, turn on a tracker and gently move it. Inspect the recent-data count, then choose **Open Device setup** to see devices under each receiver.

Click a numbered step or use **Back** and **Next** to browse. You can open every step without hardware. Advancing a step does not mark a check as passed. The guide retains your position when you visit another feature during this run. It does not save progress or a completion flag between launches.

Home shows receiver discovery and recent sensor activity from the existing read-only HID listener. Recent data does not prove that pairing, calibration, or SlimeVR setup is complete. If discovery fails, a receiver cannot be read, or another operation uses it, the summary reports that state rather than treating unknown data as zero.

Opening Home or browsing its guide, update-method chooser, and connection help does not open serial ports, send commands, query firmware, start recording, or flash devices. Existing authorized work continues when you return to Home. Choosing the **Wireless update** card opens the existing update page without changing its remembered mode. Firmware information queries start automatically in ESB mode, not Legacy BLE mode.

Use the sidebar to go directly to **Device setup**, **Firmware update**, **Wireless update**, [**Gyro calibration**](#gyro-calibration), capture, or analysis. Home also links to terminals through **Device setup**. In **Device setup**, select a device and connect its serial port when needed. Check the target before sending a terminal command or using a command card. Saved command cards and **Direct serial batch** require preview and confirmation. Terminal **Send all** submits immediately.

If a connection fails, open **Connection help** on Home. It covers missing receivers, missing tracker data, occupied serial ports, and USB-only trackers. On Linux, it also explains device-access permissions.

Before an update, prepare compatible firmware, stable power, and a USB recovery path. Cancellation cannot undo data already written or activation that has already occurred.

1. Open **Firmware update** or **Wireless update**.
2. Select the targets and the corresponding firmware.
3. Check the model, role, source, and risks.
4. Explicitly confirm the update.
5. Reconnect the devices after the update.
6. Check their firmware versions.

To analyze data, import a `.slimecap` file or a paired CSV and metadata file in **VQF replay**. Replay does not send device commands.

## Work with devices and terminals

The application groups trackers and reliably associated serial functions by receiver. Serial ports that cannot be associated reliably remain separate. Associating a receiver with a serial port requires matching USB IDs and a unique, nonempty USB serial number. Associating a tracker's direct and wireless endpoints requires a unique, normalized device address. Names, tracker IDs, and channels do not prove that two endpoints are the same device. A tracker ID is valid only within its receiver.

Use the device's command-console serial port for terminal commands. A CDC port marked **Raw data capture only** is a separate data interface, not a console. Open **Raw capture** to use it. If the console is unavailable or the association is ambiguous, check the USB connection and refresh the device list before selecting again. Do not choose a port by its number.

- At startup, the application enumerates devices and listens to the main HID interface in read-only mode. It does not open serial ports, send `info`, calibrate, or start raw capture automatically. Opening the ESB page starts read-only firmware information queries.
- Click a receiver name, available console port name, terminal tab, or online tracker card to change the current command target. Check tracker cards to work with the selected targets. Offline cards cannot join a command selection, but a uniquely identified offline pairing can keep focus for nickname editing. A connected direct serial port with a unique matching identity can also make a card appear online.
- Connect explicitly, or click a preset command or send from a disconnected terminal. The application then connects the unambiguously matched command console before it continues. The connection uses 115200 baud, 8N1, no flow control, and asserted DTR and RTS. Browsing and checking cards alone do not open ports.
- If the initial open fails, explicitly choose **Retry** or **Cancel**. The application does not take over an occupied port or resend through another channel. Close other programs that use the device or check permissions. The original error remains available.
- A session that connected successfully can recover after an observed return, one unexpected-loss probe, or a Windows COM-change notification followed by a qualifying scan. Recovery uses the same verified console, not another port, and never replays commands or retries on a timer. If no new qualifying evidence arrives, reconnect manually. See [recovery admission](ARCHITECTURE.md#terminal-recovery) for identity and scan requirements.
- **Disconnect**, **Stop reconnecting**, closing the terminal tab, device-operation handoff, and application exit revoke recovery. An initial failed open does not enable it.

Each port sends and receives independently. Switching tabs or feature pages does not close sessions. **Disconnect** retains the tab and transcript. Hover a terminal tab, or reach its close button with Tab, to reveal its **X** without moving the layout. The **X** closes connected, reconnecting, or disconnected sessions and removes the tab and transcript after cleanup. Active commands, device operations, or log export can temporarily disable it. Export any text you need before closing.

Use **Search commands** above the command list to match command text, names, or descriptions in the current language. Search is case-insensitive and keeps the current target scope. Matching groups open temporarily; clearing the search restores their previous collapsed state. Typing a search does not send a command.

The terminal has bounded scrollback, ANSI colors, output following, cross-line drag selection, plain-text copying, and ↑/↓ recall of the last 100 commands per port. Recalling a command does not send it. Drag selection pauses output following, but reception continues in the background. **Export log** saves only the retained UTF-8 text snapshot at the time you click. It is not a continuous file log, raw capture, or full VT screen emulator.

You can also browse command history with the wheel over the command input: scroll up for older commands and down for newer ones. Returning past the newest history entry restores your draft. This does not send a command; scrolling over the transcript still scrolls its output.

If **Export log** fails, an existing destination file remains unchanged.

New terminal tabs start with **Follow** and **Wrap lines** on. Wrapping fits long output to the terminal width. Each tab retains your manual choices when you switch tabs or pages. These controls affect only the display. Copy and export preserve the retained text's bytes and original line breaks.

The **Log level** menu defaults to **All**. Choose **Error**, **Warning**, **Info**, or **Debug** to show that exact level, recognized from the firmware's `<err>`, `<wrn>`, `<inf>`, and `<dbg>` prefixes. Untagged output, including command responses and continuation lines, always remains visible; a continuation does not inherit the preceding line's level. Each tab keeps its own filter until closed. Changing the filter clears the text selection but does not stop reception or delete retained text. **Copy all** and **Export log** still include all retained levels.

Scroll over the log-level control to move through its choices: down selects the next level and up the previous one. The first and last choices do not wrap around. Horizontal scrolling does not change the level.

Focus the transcript with Tab to navigate its text. Use the arrow keys to move the caret, and hold Shift to extend the selection. Home and End move to the ends of the displayed row. Page Up and Page Down scroll the transcript. On Windows and Linux, Ctrl+Home and Ctrl+End move to the displayed text's boundaries, Ctrl+A selects all displayed text, and Ctrl+C copies the selection without including filtered-out lines. There is no separate selection-copy button. Keyboard navigation pauses **Follow** without stopping reception. Turn **Follow** on again to resume following new output.

TPS is the rate of sensor records received by the host. A lack of recent data does not prove that a device is powered off and does not indicate its battery level, signal, or capabilities. A completed serial write is not a device ACK or proof of command completion.

On **Device setup**, online tracker cards show the latest HID-reported battery percentage and voltage in the upper-right corner. Devices without valid battery telemetry show no battery values. A reported `0%` remains visible. Going offline or changing the tracker assigned to a receiver slot clears the old reading.

Cards keep the name and hardware ID on two compact rows, with activity and online status below. **Current** and **Selected** badges appear at the lower right without changing the card height.

The checkbox changes only batch selection, independently of the card body's current-target action. Clicking a disabled checkbox changes neither the selection nor the current target; clicking the body of a uniquely identified offline card still opens that target.

After an observed pairing change, the application rebuilds that receiver's tracker list and clears stale selections and firmware information. Saved nicknames remain. If you changed pairings elsewhere and the list stays stale, click **Refresh devices**. Going offline alone does not remove a paired tracker.

<a id="remembered-devices"></a>
### Remember devices and give them nicknames

To rename the device you are viewing:

1. In **Device setup**, click the receiver, tracker card, or identified direct tracker console.
2. In **Commands → Current device**, check the original identity at the top of the panel.
3. Click the pencil, enter a nickname, and choose **Save**. **Cancel** discards the draft.
4. To clear the nickname, empty the input and choose **Save**.

Changing the device, command scope, or page discards an unsaved inline draft. Nickname editing does not connect a device or enable unavailable commands. If the panel offers **Remember**, choose it before editing. Devices without a unique identity have no pencil.

Open **Settings → Manage devices**, or use **Manage devices** above the Device setup roster. Remembered receivers and trackers remain listed when they are not currently observed.

1. Find the device using its original label and USB serial or RF address. Do not rely on the port or tracker number alone.
2. Choose **Edit nickname**, enter a name, and choose **Save nickname**. **Cancel** leaves the saved value unchanged.
3. To remove just the nickname, choose **Edit nickname → Clear nickname**.
4. To remove the device's saved nickname and information, choose **Forget device**, check the device identity, and confirm.

Invalid nicknames show feedback beside the draft, not a storage failure. **Cancel** clears that feedback without changing the saved value or dismissing any independent storage problem. In a long device list, Tab and Shift+Tab bring the newly focused control into view; manual scrolling is retained until focus moves again. Home, End, Page Up, and Page Down keep their editor behavior while the nickname input is focused.

Nicknames are saved only on this computer and appear beside the original device labels. Forgetting does not unpair or reset hardware, send a command, disconnect a session, or delete logs, recordings, preferences, or saved commands.

After forgetting, repeated device refreshes do not immediately add the device again. Choose **Remember** on its **Not remembered** row, restart the application, or reconnect after the application has confirmed that the device disappeared. A discovery error or missing TPS alone does not count as confirmed disappearance. Remembering again does not restore the deleted nickname.

**Currently observed** means the application currently recognizes the device, not that it is ready for commands. **Not currently observed** records remain editable. Fields marked **Last reported** describe earlier reports, not current firmware capabilities. Saved records do not connect devices.

For a tracker, **Last receiver** shows the last receiver through which this app observed unambiguous recent wireless traffic. It remains saved while devices are offline and after a restart. Old records and offline-only pairings have no receiver line until the app observes that traffic. Offline pairing lists and direct USB connections do not replace the saved receiver. The app cannot reconstruct receiver use while it was closed.

The line uses the receiver's current saved nickname or original name. Hover over it to see the full receiver identity if the text is clipped. Forgetting the receiver leaves its USB serial on the tracker but removes its saved name. This history does not indicate online status or enable commands.

Prepared command reviews and accepted history keep their captured target captions, even after a nickname or language change. Reopen the review to use the new caption. Raw terminal output, exported logs, and recording identities do not change.

Devices that cannot be uniquely identified cannot have a permanent nickname. For supported identities and technical limits, see [Remembered hardware and nickname presentation](ARCHITECTURE.md#remembered-devices).

## Send commands and batch operations

Command descriptions depend on the receiver or tracker role. An unknown role exposes only general information and help. After selecting a command, follow its parameter rules and check the full target list. Calibration shortcuts invoke firmware capabilities. They are separate from the [gyro calibration guides](#gyro-calibration). The catalog follows a pinned upstream serial contract. It provides no HID, capture, or OTA shortcut that bypasses identity, online-state, or routing checks.

Commands are grouped by their actual tasks, with one entry per command. **Device info** and **Help** appear only in **Information**, not again in **Diagnostics**. **Diagnostics** appears only when the target supports actual diagnostic commands in that group. Commands with different roles, scopes, or parameter contracts remain separate.

Click a group heading to collapse or expand its command cards. Groups start expanded. Each group keeps its state while you switch devices, command scopes, or the **History** tab during the current application session. Restarting the app expands all groups again. Folding a group does not change command parameters or send a command.

Direct tracker commands start with **Information**. Both direct and remote tracker commands then show **System**, **Diagnostics**, **Connection**, **Sensor calibration**, and **Magnetometer**, followed by **Temperature calibration** and **Reset**; remote trackers do not have an **Information** group. The remote order applies to the current remote tracker, selected trackers, and all-online commands. Receivers and devices with unknown roles still start with **Information**.

- To command selected trackers, check each card, preview every `send <slot> <command>`, and confirm. Different slots on one receiver execute separately. They are not deduplicated by serial port. A single remote card or multiple remote targets require known addresses, a unique receiver serial port, and recent sensor activity. A unique connected direct endpoint does not use this wireless-activity check.
- To command all online devices, use the receiver's **Select all online** to set the command scope, then confirm `send all <command>`. The receiver determines the actual online scope. It is neither the host's checked subset nor every historical device.
- For a direct serial batch, check serial port rows and enter the command in **Direct serial batch**. Click **Review selected…**. After the explicit targets finish connecting, click **Confirm and send**. Excluded targets do not cause an automatic change to broadcast.

Targets are frozen before parameter entry. Changing the target, selection, or scope revokes the old confirmation. An operation cannot continue if its identity, session generation, slot, or route changes. **History** retains queued, sent, and failed results for each target. A failure does not automatically change the route or resend a command that may have side effects.

### Check parameters and high-risk commands

- Channels range from 0 to 100. Remote `channel` and `clearchannel` are available only through `send all` with an explicit receiver scope. They affect the receiver and online trackers. A slot subset cannot silently expand to a broadcast.
- Accelerometer calibration uses 18 poses, not the old six-face procedure. Follow the instructions for the current firmware. Commands such as `meow` and TDMA enable or disable still depend on the device's actual firmware.
- `sens` requires finite `x,y,z` angular errors or `reset`. An empty value is not a query and is rejected. For remote commands, each axis is limited to `[-327.68,327.67]` for signed-centidegree encoding. A direct connection requires only a finite f32 triple and does not impose the remote encoding limit. The sensitivity that the device accepts still depends on its configuration and nonsingular gains. This is not a universal physical range. `sensauto` optionally accepts x, y, or z and 1–100 turns. Axis presets without a turn count use the device default.
- Remote `teston` optionally accepts 0–1000 TPS. An empty value or 0 uses the device default, and actual throughput depends on the device. Direct `teston` has no TPS argument. Do not treat the requested value as a measured rate.
- WOM entry requires confirmation. Acceptance of the command does not prove that the device is asleep. An empty WOM timeout queries the value. A value of 0 restores the default. The range is 0–35791.
- Remote reset exposes only the `zro`, `acc`, `bat`, `mag`, `tcal`, and `fusion` scopes. It does not invent `reset sens` or `reset all`. Fusion reset has a separate warning. Direct `shutdown`, `resetfusion`, and `testoff` still require you to check the exact target and risks.
- Remote `tcal` starts heating with no arguments. Confirm the experimental physical-heating risk, target-temperature configuration, external power, and hardware requirements. There are no invented remote temperature arguments, `stop`, or `status` commands. A successful send does not prove device acceptance, heating, or calibration completion. Reading or importing temperature curves does not start heating.

<a id="saved-commands"></a>
### Use saved commands and macros

#### Save a command card

1. On **Home**, open **Manage commands**.
2. Choose **New command**.
3. Enter a title.
4. Add a description or icon if needed.
5. Enter one command per line.
6. Choose a target mode:
   - To use only existing connected consoles, choose **Connected terminals**.
   - To send literal commands to chosen receivers, choose **Receiver consoles**.
   - To command online trackers through chosen receivers, choose **Trackers through receivers**.
7. To show the card on Home, select **Pin to Home**.
8. Choose **Save**.

Use at most 16 command lines. Remove interior blank lines. A final Enter does not add a step.

For **Trackers through receivers**, enter commands without a `send` envelope. The receiver determines the online scope.

Use **Edit**, **Move up**, **Move down**, **Unpin**, or **Delete** in the library to maintain saved cards. Unpinning keeps the command and its order. Built-in Home cards and the command catalog remain unchanged.

If saving fails, correct the reported storage problem, then choose **Save** again. The previous library and your draft remain available.

#### Run a saved card

Custom command effects are not verified. Check the commands and targets before sending.

1. Open a pinned card or choose **Run** in the library.
2. For receiver modes, select the receivers.
3. If a selected console is disconnected, choose **Prepare selected consoles**.
4. Review the exact commands, ports, targets, exclusions, and warning.
5. Choose **Confirm and send**.

To prevent later submissions, choose **Stop remaining steps**. Accepted writes cannot be undone. **Written** confirms transport, not firmware completion. A failed write may have transferred some bytes. Choose **X** to dismiss a completed saved-command result.

#### Send terminal input to connected consoles

1. Enter the command in the source terminal.
2. Choose **Send all** to submit immediately to the currently connected eligible consoles. There is no review or later confirmation.
3. Open **History** in the right-hand command panel to read the result. Sending does not switch tabs.

**History** includes **Send all** results from every terminal tab, labels each source terminal, and keeps existing preset command history. **Send all** results no longer appear below the terminal controls or on Home. Choose **Details** to inspect per-target results and exclusions. While sending, choose **Stop remaining steps** to prevent later submissions. Accepted writes cannot be undone. After sending finishes, choose **X** to dismiss the result. Failed or excluded results remain until you dismiss them. If the result list is full, close a completed result or wait for active sends to finish.

**Send all** never opens a connection or retries a failed write. A console that is already disconnected is not part of the new send. The draft clears only after at least one target accepts a submission, and only if its text is unchanged. Ordinary **Send** and Enter keep their single-terminal behavior.

For identity checks, per-target failures, cancellation, and the no-retry policy, see [Saved custom commands](ARCHITECTURE.md#saved-commands).

<a id="gyro-calibration"></a>
## Calibrate a tracker's gyroscope

The single **Gyro calibration** sidebar entry contains **Stationary calibration** and **Per-axis sensitivity**. Browsing, selecting a tracker, or changing the guide does not start calibration.

1. Open **Gyro calibration** and choose a guide.
2. Select one known USB tracker or one online tracker under a receiver. Check its port or receiver, slot, and tracker identity. A receiver alone, an offline tracker, or an unidentified serial device is not a target.
3. For direct USB, click **Inspect connection** on this page. Inspection opens the connection and queries identity, command support, and current parameters without starting calibration. After inspection finishes, click **Return to preparation**. You do not need to connect through **Device setup** first.
4. For **Stationary calibration**, place the tracker on a stable surface and keep it still.
5. For **Per-axis sensitivity**, choose sensor X, Y, or Z and enter 1–100 full revolutions, initially 5. Find the sensor axes in your board documentation. Do not infer them from the enclosure's edges or buttons. Unlike the ordinary `sens auto` command shortcut, the guide always sends an explicit turn count.
6. Click **Review calibration**. Check the target, task, axis, and turn count where applicable. Do not use another calibration tool or the tracker's calibration button during this run. Click **Start calibration**.
7. Follow the firmware cues. Keep still during preparation and stationary collection. For sensitivity, wait for the rotation cue, rotate the requested full turns in one direction, then stop and hold still for validation.
8. Review the result and **Details**. When the tracker reports application, use **Finish**, or **Prepare Y axis** or **Prepare Z axis** when offered. These actions wait for connection cleanup and return to preparation. They do not start another axis. Review and start each axis separately.

Switching pages does not stop or retarget a run. **Stop observing** ends the helper's observation, not calibration on the tracker. If a command may have reached the tracker, the tracker may still be calibrating and later commands may conflict or be refused.

After connection cleanup finishes, use **Acknowledge result and continue** or **Acknowledge unknown outcome and continue** when offered. Acknowledgement lets you continue in the helper. It does not change the device outcome or prove cancellation or storage. Routine direct stationary results do not require a power cycle. **Observe again without starting**, when available, requests bounded observation without another calibration command.

Ordinary direct stationary collection or queueing does not confirm application. Direct readback shows rounded current values, and wireless has no fresh coefficient readback. A reported application does not prove that coefficients were stored. Physical calibration accuracy and storage durability remain unverified. See [gyro calibration protocol limits](PROTOCOLS.md#gyro-calibration) for evidence, observation, and firmware limitations.

## Choose a firmware source

Local files, configuration presets, and embedded members are distinct sources. No-firmware packages omit embedded images, but local and remote sources remain available. **Local** and **Remote** change only the browser view. They do not clear valid assignments. Embedded firmware has a fixed version and does not follow upstream automatically.

To load a local file, open a firmware-update page and drop one file onto the window or a firmware card. You can still click **Choose local file**. For OTA, select the firmware group first. A rejected file leaves the previous selection intact. Loading does not start an update.

The remote application catalog offers Tracker repository releases, which also include receiver assets, and supported successful Tracker and Receiver CI artifacts.

For a bootloader, choose **Bootloader firmware**, then **Remote firmware**. Select **Bootloader releases** or **Bootloader CI**. The CI channel lists successful builds from the repository's `master` branch and loads only self-update UF2 files.

Click **Load versions** or **Latest available**. Choose a version and the exact board and variant. If a downloaded package has several members, select one explicitly. Latest means the newest version in that channel with artifacts still available. It does not guarantee that your board is included, flash automatically, or mean that the device is already up to date.

**Only matching boards** and variant filters narrow the list. They do not replace checks of the board, role, format, bootloader, or addresses. The currently visible variants are Standard and receiver DataCollect (CDC). The catalog does not claim to offer tracker DataCollect or HID artifacts. HEX, UF2, MCUboot BIN, Secure DFU ZIP, and Legacy DFU ZIP are not interchangeable by renaming their extensions.

Both wired and wireless **Remote firmware** lists offer **Search firmware files**. Search matches filenames and displayed variants without case sensitivity, within the existing compatibility and variant filters. Clearing it restores the list; searching does not download a file or change selected firmware.

A GitHub token is optional. Before startup, set `GH_TOKEN` or `GITHUB_TOKEN` in the process environment. A nonempty `GH_TOKEN` takes precedence. Public CI downloads try nightly.link first. Only if that fails and a token is available does the application try an authorized GitHub download. Resolve rate-limit, expiry, or permission errors, then retry explicitly. Do not put tokens in configuration files, logs, or issue attachments.

Source records retain the available version or commit, URL, and SHA-256. If upstream provides no digest, a locally computed hash is not presented as upstream authentication. Release date-based identifiers, CI runs, and device FW_INFO versions are not directly comparable. Unknown versions and dates are not inferred from file modification times.

<a id="firmware-updates"></a>
## Update firmware

UF2 and OpenDFU transfers show a progress bar, byte count, and percentage when the total is known. Preparation and other unknown-total stages use an indeterminate indicator. Reaching 100% means the reported transfer is complete; **Waiting for final result** remains until the worker finishes. It does not confirm that the new firmware has booted.

### Update through UF2

1. If the device is still running, select its command-console serial port above the flash-volume list on this page. Click **Enter bootloader**. A serial port already connected in **Device setup** reuses its existing session. You do not need to switch pages.
2. Wait for the device to enter the UF2 bootloader. Refresh the list below, check the volume label, path, and Board-ID, and select the target. Reconnecting a volume does not select it again automatically.
3. Choose **Application** or **Bootloader** and load the corresponding UF2. Bootloader updates support only explicitly matching nosd self-update packages. They require an existing compatible bootloader and are not first-time installation on a bare chip. The nice!nano → ProMicro preset is for hardware conversion, not routine self-update of arbitrary clones.
4. In **Review update**, check the targets, image, source, hash, and risks. Confirm these before **Confirm and start**. Automatic copying requires a separate selection and confirmation. It applies only to the selected volumes and image and does not authorize new volumes.
5. Reconnect after completion and check the version. **Copy completed** proves only that copying completed. A different volume, a remount, or an identity change requires a new selection and confirmation.

If the result says **Copy confirmation pending; firmware boot not verified**, reconnect and check the installed firmware or bootloader version before retrying. This result means the host accepted all bytes, but the device disconnected during finalization. It does not confirm that the new firmware started.

By default, the serial list shows eligible USB serial ports and active sessions. It identifies detected ports, not whether their terminal sessions are open. Use **Show other serial ports** to see other eligible ports. Raw-data CDC and ambiguous receiver ports remain excluded. After selection, inspect the system-provided product, manufacturer, USB ID, and serial number, together with information reported by a connected device. A device that cannot be identified reliably requires additional confirmation. A new connection opens the serial port only after confirmation. Connection or send failures show the reason and available recovery advice in the current area. They do not trigger automatic resends. This entry sends only `dfu`. It does not guarantee firmware support, select a flash volume, or start an update automatically. Holyiot still uses the dedicated procedure below.

**Exit DFU** requires confirmation of the selected volume. It copies only that volume's own valid `CURRENT.UF2`, unchanged, back as `REBOOT.UF2`. A missing or damaged CURRENT file is rejected. The pending firmware image is not substituted. A successful copy does not prove a physical reboot.

### Update Holyiot through OpenDFU

1. Select the Holyiot application's command-console serial port and confirm **Enter Holyiot DFU**. If commands are still sending, wait for them to finish and try again. Once the action is accepted, the application waits for the original terminal to close before handing over the port.
2. Refresh after re-enumeration. Manually select the **Open DFU Bootloader** port again. Do not treat the old COM port as the new bootloader's identity.
3. Load the matching HEX or Nordic Secure DFU ZIP. Check the preview and confirm. Defaults are application base `0x1000`, hw `52`, app `1`, and sd requirement `0`. Other HEX files are not silently relocated. Configuration can supply suitable parameters.
4. Reconnect after completion and check the version. An `nrf5_opendfu` receiver does not update itself through ESB, but can still relay tracker OTA.

If a Holyiot receiver cannot enter its bootloader by command, gently touch the indicator above the connector with the supplied magnet. Then continue from step 2.

The same pending-command check applies when starting OpenDFU. A rejected attempt does not interrupt accepted commands or retry automatically.

### Update through ESB and HID OTA

1. Open the ESB page in **Wireless update**. The application automatically queries the receiver and online trackers with known addresses in read-only mode. It does not select them automatically. Successful information is cached for 30 seconds. Staying on the page does not start periodic polling. Query failures appear on the affected device card, not as duplicate top-level results. Refresh the whole receiver or retry an individual failed tracker. Switching pages does not silently retry failures. Queries that have started continue after you leave the page. Only explicit cancellation requests that they stop.
2. Choose embedded, local, or remote images for each model and role group. Select a member explicitly when a package contains several. Initial grouping attempts to assign compatible embedded images without overwriting manual choices. No-firmware builds do not repeatedly try to read missing embedded images. OTA does not update the bootloader.
3. Check each receiver and target, the installed information, and the assigned file and source. Do not infer an upgrade or downgrade from names. **Back** and **Edit** retain valid selections but revoke confirmation.
4. Review and freeze the final targets, then confirm the update. Trackers update before the receiver itself. Results are retained per target, and the operation does not expand to a broadcast. After completion, use **New update** to start another workflow. You must confirm again.

After transferring and verifying a receiver image, the helper attempts activation once and uses a three-second observation budget. If no completion confirmation arrives, the operation ends with **Transfer complete; activation not confirmed** rather than waiting for a prolonged reconnect. Keep the receiver powered; this result does not mean that the update failed. When a later device scan identifies the same receiver, the result can show **Receiver detected after update**. Neither that status nor **Receiver responding** proves that the selected image or version is installed. Select **New update** to return to the device list. Use **Refresh firmware information** to inspect the installed version before deciding whether to retry.

Query results and images selected while offline do not authorize an update. Submission still requires fresh online-state, identity, and compatibility checks. Ordinary page queries get one attempt with a 3-second response budget. Execution preflight can repeat an incomplete query once under the target-specific conditions in [ESB OTA protocols](PROTOCOLS.md#esb-ota). Progress uses acknowledged bytes. VERIFY_OK, ACTIVATING, or a successful write does not replace COMPLETE. COMPLETE does not prove that the new firmware started normally. If a wireless update fails, connect through USB and recover in **Firmware update**.

### Use Legacy BLE and observe shared safety limits

Legacy BLE OTA appears only when **Advanced features** is enabled in settings. First, put the device into Legacy BLE DFU mode manually. Scan and select the actual devices. Choose a local Nordic Legacy application ZIP or a suitable preset, inspect the preview, and confirm. There is no remote source selector. HEX and Secure DFU ZIP cannot replace a Legacy package. Disabling advanced features does not interrupt an in-flight BLE task.

For a locally built image with an unknown board, you can explicitly accept the compatibility risk only after all other checks pass. Known conflicts are still rejected. Updates to unknown remote targets are rejected. An update takes exclusive control of the affected devices and waits for draining, closure, cleanup, and worker join. Unrelated terminals are expected to continue, but continuity during real multi-device flashing still awaits acceptance testing.

Cancellation does not roll back an update. **Cancel** or application exit requests a stop and waits for resources to be released. Neither can undo data already written, completed targets, or activation. A device may remain in its bootloader or a partially updated state. Keep power connected, inspect each target's error, and use the appropriate USB recovery procedure. Full physical transfer, activation, reboot-version checks, and interruption recovery have not completed acceptance testing.

## Inspect HID data and events

This page reuses existing readers and keeps the main and raw interfaces separate. Opening it does not start raw capture or put trackers to sleep. Multi-select filters cover receiver, tracker ID, category, and event type. Choices within one group use OR. Groups combine with AND. An empty group imposes no limit. Filtering sends no commands.

Explicitly select a receiver and subscribe to events. The application queries capabilities, subscribes, and renews the subscription every 5 seconds. It waits at most 2 seconds for an ACK. **Stop all event subscriptions** revokes every request owned by this application, including recovery intent for disconnected or occupied devices. It does not stop recording. Subscriptions that you have not explicitly stopped can resume after OTA or raw capture returns the device. Unsupported or failed operations require an explicit retry. An operation ACK is not proof of calibration completion.

Pausing the display pauses only the list, not file recording. **Save recent log** freezes the retained reports. **Record to file** saves continuously and independently of the page or display filters. **Stop recording and save** drains pending data, writes the end record, and synchronizes the file. You can record again after completion. Existing files cannot be overwritten. A `.hidlog` contains the original HID records actually read or written by this application. It is not a system USB capture, serial log, or `.slimecap`. Offline playback and virtual HID are not supported yet. See [Recording formats](PROTOCOLS.md#recording-formats).

Focus the report list with Tab. Use the arrow keys to select a report, Page Up and Page Down to move a page, or Home and End to reach the first or last filtered report. The selected report stays visible as you navigate.

## Capture raw data

1. Choose the receiver's **Raw HID** or **CDC binary data** source. The CDC list contains only uniquely identified raw-data interfaces, not command consoles or arbitrary USB serial ports. The presence of an interface does not prove firmware support. Main-HID fused poses are not a raw stream.
2. Choose one tracker or all paired trackers and set the requested rate. Single-device mode may put other trackers paired to the same receiver to sleep. Explicit confirmation is required. Batch mode is not an arbitrary checked subset.
3. Choose a new `.slimecap` file and start. Inspect warm-up, samples, previews, and **Details** warnings. After stopping, wait for STOP, tail draining, and worker completion before opening replay.

In single-tracker mode, use the cards to check online status and TPS before selecting a tracker. The activity outline follows TPS. This is recent main-HID traffic, not the requested raw sampling rate. Paired offline trackers remain selectable. During capture, the page shows the frozen target instead of stale activity cards.

If no source appears, check compatible firmware and the USB connection, then refresh. CDC stays unavailable when the raw-data interface cannot be identified uniquely.

Capture takes exclusive control of the affected HID or CDC interfaces and blocks related commands and updates. Switching pages does not release them. Unrelated devices are expected to remain usable. Physical-device START and ACK, throughput, sample loss, STOP, hotplug, and continuity still await acceptance testing.

## Replay data with VQF

Import a `.slimecap`, a CSV with its same-named `.meta.txt`, or a ZIP containing the pair. Select a track to start automatic background computation. You can also open a completed capture directly. Replay, parameter edits, and export do not write to the device.

Imports that exceed the [recording limits](PROTOCOLS.md#recording-formats) are rejected, including oversized CSV records.

- The top bar separately controls accelerometer calibration, magnetometer calibration, gyro **None**, **Bias**, or **Thermal**, and magnetometer use. Only calibrations present in the data can be enabled. **Parameters** contains 25 VQF parameters in groups. Valid edits trigger a debounced recomputation. While computation runs, old results are not presented as results for the new settings.
- Synchronized charts on the left depend on observations, calibration, and switches. The pose and diagnostics on the right scroll independently. Missing temperature or magnetic data does not produce zero curves. Hover values come from original replay frames, not interpolation of downsampled lines.
- Left-drag a curve or use the bottom slider to seek. Use Shift+drag to select a time range, Ctrl+wheel to zoom, and the wheel alone to scroll. The chart toolbar also provides **Zoom in**, **Zoom out**, **Earlier**, **Later**, and **Fit**. Double-click restores the full range. Drag the model to rotate the camera and use the wheel to zoom. With the model focused, arrow keys rotate the camera, + and − zoom, and Home resets the camera. These model controls do not move the playback cursor.
- The bottom bar supports looping, 0.25–8× speed, 6D and 9D, sample stepping, and **Robust** or **Full Y**. It indicates when extreme values are clipped. When you are not editing an input and no settings await computation, use Space, the arrow keys, or A/B/X. Playback continues when you switch to another application. Switching feature pages within this application pauses it.
- The first successful result attempts **Auto A/B** once. You can also set **A**, **B**, or **Clear** manually, or explicitly select references again. Recomputing the same file preserves valid references and zoom. Changing sources resets them. Auto A/B requires a single continuous segment, at least 60 seconds, and distinct qualifying stationary intervals. If these conditions fail, it does not partially apply references. Metrics are not ground truth. Continuous drift is not calculated across filter resets.
- Choose built-in parameters or save, rename, and delete custom presets. **Import YAML** strictly merges a partial set of camelCase parameters. **Export YAML** exports all parameters. YAML does not include recordings, calibration, rates, references, or the camera. Use **Export ZIP** to export a recording as CSV with paired metadata. A damaged preset store is not silently overwritten.

Cancelled or failed **Export ZIP** operations leave an existing destination unchanged. Failed **Export YAML** operations also preserve the existing destination.

For the kernel, coordinates, missing-data behavior, and numerical limits, see [Protocols and sources](PROTOCOLS.md). The pose model is an illustrative preview, not precise CAD or a complete calibration workspace. Bit-for-bit agreement with a running device is not guaranteed.

<a id="thermal-dump"></a>
## Read and analyze temperature calibration curves

Temperature curves use a separate, read-only analysis window for existing human-readable `tcal dump` output. This does not use the sidebar entry reserved for a future temperature-calibration assistant and is not a complete heating or calibration wizard.

1. Open **Tools** in the sidebar. Click **Temperature calibration analysis** to open or focus the single analysis window. The main window stays on **Tools**. You can import saved dump text without a connected device. Opening this tool sends no command.
2. In **Device setup**, enter `tcal dump` in a directly connected tracker's serial terminal, or click its preset command. A complete, valid response automatically opens or updates the same analysis window. It does not change the main window's feature page, active terminal, or device selection. From the analysis window, you can also explicitly read a direct tracker serial connection that is already connected and has a confirmed role. This does not open another serial handle, connect automatically, broadcast remotely, start heating or calibration, or issue raw START. Incomplete or erroneous responses produce explicit errors. A device with no points can produce a valid empty result.
3. A wide window places three nearly square X, Y, and Z charts side by side. A narrow window stacks them. In a short window, scroll down to see statistics and legends. The charts share a temperature viewport in °C, with an independent bias axis in °/s for each chart. Circles mark raw points, green is the global linear fit, and blue is the local MLS fit. A continuous band along MLS shows ±0.01°/s. Dashed lines show boundary extrapolation, at most 6°C on each side. The band is not a fit confidence interval. Beside each axis, inspect its bias range and the average, maximum, and minimum adjacent-point slopes. The legend reports the fit's residual sum of squares. Negative temperatures are not clipped to 0°C. Insufficient points do not produce invented fits.
4. The toolbar shows the source filename and provides import, **Zoom in**, **Zoom out**, pan arrows, **Reset view**, and **Save chart SVG**. SVG export uses three columns. It freezes the analysis and viewport when you click save, so data changes while you choose a destination do not change the chart being saved. Closing the analysis window does not exit the application, disconnect serial ports, or cancel a read that has started. Reopening retains the latest document and viewport. Exiting the main application closes the analysis window and follows the application's device-task cleanup procedure. Language and theme changes follow application settings immediately.

If **Save chart SVG** fails, an existing destination file remains unchanged.

Firmware console numbers are rounded. A dump is not a lossless temperature-calibration backup, raw binary snapshot, or configuration that can be written back. Analysis preserves sparse slot order. Import allows at most 512 points, slots 0–511, 4096 raw bytes per line, and 256 KiB per response. The header, footer, and row count must agree.

After cancellation, timeout, or an early parse failure in a started but incomplete request, that port enters quarantine. Explicitly disconnect and reconnect it. The protocol has no nonce, so a late response cannot be assigned to a new request. Normal success, an empty TCAL response, or a count error after a complete terminator line has been consumed does not require reconnection for this reason. The generic `Command unavailable in this firmware` response does not prove that a dump has ended. An online read continues until the original deadline. If no dump response arrives, the read fails and requires reconnection. Offline import can report the unsupported command immediately. Fits are diagnostic only and are not written to the device. For the mathematics, see [Temperature dump protocols](PROTOCOLS.md#thermal-dump).

<a id="rssi-scan"></a>
## Analyze RSSI channels

1. In **Device setup**, run `rssi_scan` in the receiver's serial terminal, or use the preset **RSSI scan** command. Valid measurements automatically open a separate chart window. Later data updates the chart without changing the main window's page, terminal, or device selection.
2. Each measured channel has a bar at its actual numeric position on the horizontal axis, ending at its measured RSSI. The vertical dBm scale is automatically padded and rounded around observed samples; each bar starts at the current lower labeled tick. New live data can rescale the axis and bars. Lower dBm values indicate quieter channels, but dBm is logarithmic and the auto-scaled, truncated baseline means bar heights are not power ratios. Unsampled or excluded integer channels within the reported range have small, neutral, low-contrast × marks near the baseline, matching the **No sample** legend; these indicate no usable sample, not a measured zero or signal height. Actual channel positions and gaps remain intact, and ordinary spaces between columns remain clear. The chart marks the current channel and the receiver's recommended channel independently. Hovering a measured bar immediately shows its channel and dBm reading; hovering anywhere in a missing channel's full-height column immediately shows its channel and **No sample**. Use **Previous sample** and **Next sample** to keep an exact channel reading visible without hovering. These controls skip channels without measurements. The application does not change the operating channel automatically.
3. You can also open **Tools → RSSI channel analysis** and import a saved scan log as UTF-8 text, up to 256 KiB. Closing the chart does not affect the serial connection. Reopening retains the latest result. Later data from the same scan does not repeatedly reopen the window.

This comparison uses the strongest RSSI observed on each channel during the scan, not tracker connection quality or average signal. If trackers are in use during the scan, normal communication can make the current channel more active. That does not necessarily mean external interference. The chart displays this warning at the bottom. Unmeasured and invalid channels are not filled with zeros. For protocol and data limits, see [RSSI scan protocols](PROTOCOLS.md#rssi-scan).

## Change settings and handle preference errors

Open **Settings** at the bottom of the sidebar.

- Search setting names and descriptions in the left column. Use **Clear search** to restore the unfiltered list.
- Choose **General**, **Appearance**, **Devices**, or **About** to jump to that group.
- Under **Language / 语言**, choose **English** or **简体中文**. Choose **Light** or **Dark** directly. **Theme color** shows every name with a color swatch; checkmarks identify the selected choices. Use Tab or Shift+Tab to move between choices and Enter or Space to select. Drag the divider to resize the settings navigation.
- Open **Manage devices** in **Devices** to edit remembered hardware. Open **View licenses and notices** in **About** to read the bundled notices.
- Under **About**, use **Project repositories** to open the application, receiver firmware, tracker firmware, or bootloader repository in your browser.

Language, light or dark appearance, theme color, and advanced features take effect immediately. Preferences are saved in the platform's user configuration directory, not the source directory. Without an explicit language choice, Chinese systems use Simplified Chinese and other systems use English. An explicit setting takes precedence. Damaged, unsupported, unreadable, oversized, or non-regular preference inputs are not overwritten. If saving fails, the existing preference file remains intact. The application reports that the change applies only to this session and offers a retry. Switching pages or themes does not recreate communication sessions.

Saving runs in the background. Normal application exit waits for the latest preference change to finish saving.

In **Settings**, select **View licenses and notices** to read the bundled original document offline. **Copy all** copies the original embedded text without reformatting. The document and [license overview](../README.md#license) distinguish application-owned MIT OR Apache-2.0 code from separate third-party, firmware, native runtime, and toolchain terms.

Use the right scrollbar to move through the document. For long lines, use the bottom scrollbar. The viewer preserves the original line breaks.

Compilation generates the embedded `THIRD_PARTY_NOTICES.txt` with the shared Rust notice library; Python is not needed to build or run the application. AppImage also carries this file under `usr/share/licenses/slimenrf-helper` inside its AppDir, and Flatpak installs it under `/app/share/licenses/slimenrf-helper`. To generate a separate copy from the tracked sources, see the [notice-maintenance procedure](DEVELOPMENT.md#license-notices).

<a id="app-updates"></a>
## Check for application updates

**Settings → About** can check GitHub releases on all platforms. Only eligible official Windows portable builds can install updates. Other platforms and Windows development, custom, or unmarked builds open the release page for manual installation.

1. Choose **Check for updates**. Stable releases are selected by default; enable **Include prereleases** to include previews. If the repository has only prereleases, the stable channel reports that no stable release is available.
2. Review the version and release notes. On check-only builds, choose **View release on GitHub** to open that release in your browser. The application does not download or replace its executable. A newer release does not necessarily contain an installation package for your platform.
3. On eligible Windows portable builds, choose **Download update**. The asset must exactly match the Windows architecture and bundled/no-firmware variant; missing assets do not cause a variant switch.
4. Downloading and a ready update keep navigation on the update settings. **Cancel** discards the update and restores navigation, including when you need to return to an active device task. Checks alone do not restrict navigation. Closing the application normally discards the download without installing it.
5. Finish or cancel active firmware, capture, and calibration work, then choose **Restart and install** and confirm. The application enters its normal shutdown flow, stops background activity, and waits for retained work before preparing the installer. A separate helper waits for the application to exit, replaces the executable, and reopens it with the original arguments and working directory. Renamed executables are supported. Configuration and preferences are not replaced.

**Check for updates automatically** is enabled by default for eligible official Windows builds, with at least 24 hours between recorded attempts. Automatic checks wait for the attempt timestamp to be saved; if saving fails, use a manual check. Automatic checks do not download or install anything. Development builds and other platforms do not check automatically.

Checks use HTTPS from the fixed project GitHub repository. Windows in-app downloads must also match GitHub's declared size and SHA-256; this is integrity verification, not an independent publisher signature. Installation requires a writable location and never requests elevation. Preparation errors leave the installed executable unchanged, but shutdown has already begun; after dismissing the error, start the application again manually. Replacement or process-launch failure attempts to restore the old executable and reports any recovery failure. A new process that launches successfully but later fails during application startup is not automatically rolled back. If it does not acknowledge the cleanup handoff, it is not forcibly terminated: recovery files are retained and their location is reported.

Install the first release containing this feature manually; older releases cannot gain an updater without being replaced. You can always download a release and replace the executable manually after closing the application.

<a id="linux-devices"></a>
## Linux device and runtime requirements

Linux needs a working X11 or Wayland environment, Vulkan drivers and an ICD, fonts, a user D-Bus, and the appropriate portal backend for file selection. Legacy BLE also needs BlueZ, an adapter, and permissions. AppImage does not bypass these requirements or guarantee compatibility with old glibc versions or every distribution.

Do not run the GUI as root or grant everyone write access to every hidraw or tty device. The repository's optional rule matches only receiver `1209:7690`. Have an administrator explicitly install it, then unplug and reconnect the device:

```bash
sudo install -m 0644 ./scripts/70-slimenrf-helper.rules /etc/udev/rules.d/70-slimenrf-helper.rules
sudo udevadm control --reload-rules
```

Run these commands from the repository root. If you downloaded only the application, obtain the [rule file](../scripts/70-slimenrf-helper.rules) separately. `uaccess` depends on a local desktop session. It does not guarantee ACL access through SSH, in the background, or in WSL. For other VID/PID values, configure a narrowly matched rule or the distribution's serial-port group. Log in again after group changes. Do not run a global `udevadm trigger`.

Before UF2 use, a desktop automounter or the user must mount the volume as a writable FAT block-device filesystem. The application does not mount it automatically. A temporary directory containing INFO_UF2.TXT, a network share, a Windows drive letter, or `/mnt/<drive-letter>` does not replace a native Linux UF2 mount. The application checks the mount instance again before writing. A remount requires a new selection and confirmation.

A WSL build is a Linux application. Its GUI needs WSLg and a renderer. USB also needs forwarding, drivers, and permissions. A working WSLg window does not prove BLE support, physical flashing, hotplug, or native-distribution compatibility. For release and hardware requirements, see [CI and release acceptance](DEVELOPMENT.md#ci-and-release-acceptance).
