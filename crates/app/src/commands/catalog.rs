use super::{ALL, BOTH, CommandContext, ParameterSpec, Preset, RECEIVER, TRACKER};

const CHANNEL: ParameterSpec = ParameterSpec::Integer {
    min: 0,
    max: 100,
    default: "0",
};
const SENS_AUTO: ParameterSpec = ParameterSpec::SensitivityAuto;
const RESET: ParameterSpec = ParameterSpec::RemoteReset;

macro_rules! entry {
    (@scope) => { false };
    (@scope receiver_wide) => { true };
    ($id:literal, $group:literal, $label:literal, $description:literal, $command:literal, $parameter:expr, $roles:expr, $remote:expr, $danger:expr $(, $scope:ident)?) => {
        Preset {
            id: $id,
            group: $group,
            label: $label,
            description: $description,
            command: $command,
            parameter: $parameter,
            roles: $roles,
            remote_eligible: $remote,
            receiver_wide_only: entry!(@scope $($scope)?),
            dangerous: $danger,
        }
    };
}

// Confirm disruptive operations independently of their presentation tone.
// Applicability follows the pinned tracker console and receiver serial parser.
pub(super) static PRESETS: &[Preset] = &[
    entry!(
        "info",
        "commands.group.information",
        "commands.preset.info.title",
        "commands.preset.info.description",
        "info",
        ParameterSpec::None,
        ALL,
        false,
        false
    ),
    entry!(
        "help",
        "commands.group.information",
        "commands.preset.help.title",
        "commands.preset.help.description",
        "help",
        ParameterSpec::None,
        ALL,
        false,
        false
    ),
    entry!(
        "uptime",
        "commands.group.information",
        "commands.preset.uptime.title",
        "commands.preset.uptime.description",
        "uptime",
        ParameterSpec::None,
        BOTH,
        false,
        false
    ),
    entry!(
        "battery",
        "commands.group.information",
        "commands.preset.battery.title",
        "commands.preset.battery.description",
        "battery",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "list",
        "commands.group.information",
        "commands.preset.list.title",
        "commands.preset.list.description",
        "list",
        ParameterSpec::None,
        RECEIVER,
        false,
        false
    ),
    entry!(
        "calibrate",
        "commands.group.sensor_calibration",
        "commands.preset.calibrate.title",
        "commands.preset.calibrate.description",
        "calibrate",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "calibrate-acc",
        "commands.group.sensor_calibration",
        "commands.preset.calibrate_acc.title",
        "commands.preset.calibrate_acc.description",
        "calibrate acc",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "sens",
        "commands.group.sensor_calibration",
        "commands.preset.sens.title",
        "commands.preset.sens.description",
        "sens",
        ParameterSpec::Sensitivity { remote: false },
        TRACKER,
        false,
        false
    ),
    entry!(
        "sens-reset",
        "commands.group.sensor_calibration",
        "commands.preset.sens_reset.title",
        "commands.preset.sens_reset.description",
        "sens reset",
        ParameterSpec::None,
        TRACKER,
        false,
        true
    ),
    entry!(
        "remote-sens",
        "commands.group.sensor_calibration",
        "commands.preset.remote_sens.title",
        "commands.preset.remote_sens.description",
        "sens",
        ParameterSpec::Sensitivity { remote: true },
        0,
        true,
        false
    ),
    entry!(
        "sens-auto",
        "commands.group.sensor_calibration",
        "commands.preset.sens_auto.title",
        "commands.preset.sens_auto.description",
        "sens auto",
        SENS_AUTO,
        TRACKER,
        true,
        false
    ),
    entry!(
        "set",
        "commands.group.connection",
        "commands.preset.set.title",
        "commands.preset.set.description",
        "set",
        ParameterSpec::Text {
            hint: "commands.parameter.hint.connection"
        },
        TRACKER,
        false,
        false
    ),
    entry!(
        "tracker-pair",
        "commands.group.connection",
        "commands.preset.tracker_pair.title",
        "commands.preset.tracker_pair.description",
        "pair",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "tracker-clear",
        "commands.group.connection",
        "commands.preset.tracker_clear.title",
        "commands.preset.tracker_clear.description",
        "clear",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "tracker-channel",
        "commands.group.connection",
        "commands.preset.tracker_channel.title",
        "commands.preset.tracker_channel.description",
        "channel",
        CHANNEL,
        TRACKER,
        true,
        false,
        receiver_wide
    ),
    entry!(
        "tracker-clearchannel",
        "commands.group.connection",
        "commands.preset.tracker_clearchannel.title",
        "commands.preset.tracker_clearchannel.description",
        "clearchannel",
        ParameterSpec::None,
        TRACKER,
        true,
        true,
        receiver_wide
    ),
    entry!(
        "tdma-on",
        "commands.group.connection",
        "commands.preset.tdma_on.title",
        "commands.preset.tdma_on.description",
        "tdma on",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tdma-off",
        "commands.group.connection",
        "commands.preset.tdma_off.title",
        "commands.preset.tdma_off.description",
        "tdma off",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tcal-status",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_status.title",
        "commands.preset.tcal_status.description",
        "tcal status",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "tcal-dump",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_dump.title",
        "commands.preset.tcal_dump.description",
        "tcal dump",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "tcal-clear",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_clear.title",
        "commands.preset.tcal_clear.description",
        "tcal clear",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "tcal-on",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_on.title",
        "commands.preset.tcal_on.description",
        "tcal on",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tcal-off",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_off.title",
        "commands.preset.tcal_off.description",
        "tcal off",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tcal-auto-on",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_auto_on.title",
        "commands.preset.tcal_auto_on.description",
        "tcal auto on",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tcal-auto-off",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_auto_off.title",
        "commands.preset.tcal_auto_off.description",
        "tcal auto off",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tcal-boot-on",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_boot_on.title",
        "commands.preset.tcal_boot_on.description",
        "tcal boot on",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tcal-boot-off",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_boot_off.title",
        "commands.preset.tcal_boot_off.description",
        "tcal boot off",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tcal-heat-start",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_heat_start.title",
        "commands.preset.tcal_heat_start.description",
        "tcal heat start",
        ParameterSpec::None,
        0,
        true,
        true
    ),
    entry!(
        "tcal-remove",
        "commands.group.temperature_calibration",
        "commands.preset.tcal_remove.title",
        "commands.preset.tcal_remove.description",
        "tcal remove",
        ParameterSpec::Text {
            hint: "commands.parameter.hint.remove_entry"
        },
        TRACKER,
        false,
        true
    ),
    entry!(
        "mag",
        "commands.group.magnetometer",
        "commands.preset.mag.title",
        "commands.preset.mag.description",
        "mag",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "mag-on",
        "commands.group.magnetometer",
        "commands.preset.mag_on.title",
        "commands.preset.mag_on.description",
        "mag on",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "mag-off",
        "commands.group.magnetometer",
        "commands.preset.mag_off.title",
        "commands.preset.mag_off.description",
        "mag off",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "mag-auto-on",
        "commands.group.magnetometer",
        "commands.preset.mag_auto_on.title",
        "commands.preset.mag_auto_on.description",
        "mag auto on",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "mag-auto-off",
        "commands.group.magnetometer",
        "commands.preset.mag_auto_off.title",
        "commands.preset.mag_auto_off.description",
        "mag auto off",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "mag-cal",
        "commands.group.magnetometer",
        "commands.preset.mag_cal.title",
        "commands.preset.mag_cal.description",
        "mag cal",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "mag-clear",
        "commands.group.magnetometer",
        "commands.preset.mag_clear.title",
        "commands.preset.mag_clear.description",
        "mag clear",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "scan",
        "commands.group.diagnostics",
        "commands.preset.scan.title",
        "commands.preset.scan.description",
        "scan",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "debug",
        "commands.group.diagnostics",
        "commands.preset.debug.title",
        "commands.preset.debug.description",
        "debug",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "debug-timed",
        "commands.group.diagnostics",
        "commands.preset.debug_timed.title",
        "commands.preset.debug_timed.description",
        "debug",
        ParameterSpec::Text {
            hint: "commands.parameter.hint.debug_duration"
        },
        TRACKER,
        false,
        false
    ),
    entry!(
        "ping",
        "commands.group.diagnostics",
        "commands.preset.ping.title",
        "commands.preset.ping.description",
        "ping",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "meow",
        "commands.group.diagnostics",
        "commands.preset.meow.title",
        "commands.preset.meow.description",
        "meow",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "range",
        "commands.group.diagnostics",
        "commands.preset.range.title",
        "commands.preset.range.description",
        "range",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "range-reset",
        "commands.group.diagnostics",
        "commands.preset.range_reset.title",
        "commands.preset.range_reset.description",
        "range reset",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "test-on",
        "commands.group.diagnostics",
        "commands.preset.test_on.title",
        "commands.preset.test_on.description",
        "test on",
        ParameterSpec::OptionalInteger {
            min: 0,
            max: 1000,
            hint: "commands.parameter.hint.tps"
        },
        0,
        true,
        false
    ),
    entry!(
        "test-off",
        "commands.group.diagnostics",
        "commands.preset.test_off.title",
        "commands.preset.test_off.description",
        "test off",
        ParameterSpec::None,
        TRACKER,
        true,
        false
    ),
    entry!(
        "tracker-test-on",
        "commands.group.diagnostics",
        "commands.preset.tracker_test_on.title",
        "commands.preset.tracker_test_on.description",
        "test on",
        ParameterSpec::None,
        TRACKER,
        false,
        false
    ),
    entry!(
        "reset-zro",
        "commands.group.reset",
        "commands.preset.reset_zro.title",
        "commands.preset.reset_zro.description",
        "reset zro",
        ParameterSpec::None,
        TRACKER,
        false,
        true
    ),
    entry!(
        "reset-acc",
        "commands.group.reset",
        "commands.preset.reset_acc.title",
        "commands.preset.reset_acc.description",
        "reset acc",
        ParameterSpec::None,
        TRACKER,
        false,
        true
    ),
    entry!(
        "reset-sens",
        "commands.group.reset",
        "commands.preset.reset_sens.title",
        "commands.preset.reset_sens.description",
        "reset sens",
        ParameterSpec::None,
        TRACKER,
        false,
        true
    ),
    entry!(
        "reset-tcal",
        "commands.group.reset",
        "commands.preset.reset_tcal.title",
        "commands.preset.reset_tcal.description",
        "reset tcal",
        ParameterSpec::None,
        TRACKER,
        false,
        true
    ),
    entry!(
        "reset-mag",
        "commands.group.reset",
        "commands.preset.reset_mag.title",
        "commands.preset.reset_mag.description",
        "reset mag",
        ParameterSpec::None,
        TRACKER,
        false,
        true
    ),
    entry!(
        "reset-bat",
        "commands.group.reset",
        "commands.preset.reset_bat.title",
        "commands.preset.reset_bat.description",
        "reset bat",
        ParameterSpec::None,
        TRACKER,
        false,
        true
    ),
    entry!(
        "reset-all",
        "commands.group.reset",
        "commands.preset.reset_all.title",
        "commands.preset.reset_all.description",
        "reset all",
        ParameterSpec::None,
        TRACKER,
        false,
        true
    ),
    entry!(
        "remote-reset",
        "commands.group.reset",
        "commands.preset.remote_reset.title",
        "commands.preset.remote_reset.description",
        "reset",
        RESET,
        0,
        true,
        true
    ),
    entry!(
        "reset-fusion",
        "commands.group.reset",
        "commands.preset.reset_fusion.title",
        "commands.preset.reset_fusion.description",
        "reset fusion",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "reboot",
        "commands.group.system",
        "commands.preset.reboot.title",
        "commands.preset.reboot.description",
        "reboot",
        ParameterSpec::None,
        BOTH,
        true,
        true
    ),
    entry!(
        "dfu",
        "commands.group.system",
        "commands.preset.dfu.title",
        "commands.preset.dfu.description",
        "dfu",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "dfu-ota",
        "commands.group.system",
        "commands.preset.dfu_ota.title",
        "commands.preset.dfu_ota.description",
        "dfu ota",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "shutdown",
        "commands.group.system",
        "commands.preset.shutdown.title",
        "commands.preset.shutdown.description",
        "shutdown",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "wom-enter",
        "commands.group.system",
        "commands.preset.wom_enter.title",
        "commands.preset.wom_enter.description",
        "wom enter",
        ParameterSpec::None,
        TRACKER,
        true,
        true
    ),
    entry!(
        "wom-timeout",
        "commands.group.system",
        "commands.preset.wom_timeout.title",
        "commands.preset.wom_timeout.description",
        "wom timeout",
        ParameterSpec::OptionalInteger {
            min: 0,
            max: 35791,
            hint: "commands.parameter.hint.wom_timeout"
        },
        TRACKER,
        true,
        false
    ),
    entry!(
        "add",
        "commands.group.management",
        "commands.preset.add.title",
        "commands.preset.add.description",
        "add",
        ParameterSpec::Text {
            hint: "commands.parameter.hint.pairing_data"
        },
        RECEIVER,
        false,
        false
    ),
    entry!(
        "remove",
        "commands.group.management",
        "commands.preset.remove.title",
        "commands.preset.remove.description",
        "remove",
        ParameterSpec::None,
        RECEIVER,
        false,
        true
    ),
    entry!(
        "receiver-pair",
        "commands.group.management",
        "commands.preset.receiver_pair.title",
        "commands.preset.receiver_pair.description",
        "pair",
        ParameterSpec::None,
        RECEIVER,
        false,
        false
    ),
    entry!(
        "pair-count",
        "commands.group.management",
        "commands.preset.pair_count.title",
        "commands.preset.pair_count.description",
        "pair",
        ParameterSpec::Text {
            hint: "commands.parameter.hint.pair_count"
        },
        RECEIVER,
        false,
        false
    ),
    entry!(
        "exit",
        "commands.group.management",
        "commands.preset.exit.title",
        "commands.preset.exit.description",
        "exit",
        ParameterSpec::None,
        RECEIVER,
        false,
        false
    ),
    entry!(
        "receiver-clear",
        "commands.group.management",
        "commands.preset.receiver_clear.title",
        "commands.preset.receiver_clear.description",
        "clear",
        ParameterSpec::None,
        RECEIVER,
        false,
        true
    ),
    entry!(
        "stats",
        "commands.group.statistics",
        "commands.preset.stats.title",
        "commands.preset.stats.description",
        "stats",
        ParameterSpec::None,
        RECEIVER,
        false,
        false
    ),
    entry!(
        "stats-timed",
        "commands.group.statistics",
        "commands.preset.stats_timed.title",
        "commands.preset.stats_timed.description",
        "stats",
        ParameterSpec::Text {
            hint: "commands.parameter.hint.statistics_duration"
        },
        RECEIVER,
        false,
        false
    ),
    entry!(
        "resetstats",
        "commands.group.statistics",
        "commands.preset.resetstats.title",
        "commands.preset.resetstats.description",
        "resetstats",
        ParameterSpec::None,
        RECEIVER,
        false,
        false
    ),
    entry!(
        "receiver-channel",
        "commands.group.radio",
        "commands.preset.receiver_channel.title",
        "commands.preset.receiver_channel.description",
        "channel",
        CHANNEL,
        RECEIVER,
        false,
        false
    ),
    entry!(
        "receiver-clearchannel",
        "commands.group.radio",
        "commands.preset.receiver_clearchannel.title",
        "commands.preset.receiver_clearchannel.description",
        "clearchannel",
        ParameterSpec::None,
        RECEIVER,
        false,
        true
    ),
    entry!(
        "rssi-scan",
        "commands.group.radio",
        "commands.preset.rssi_scan.title",
        "commands.preset.rssi_scan.description",
        "rssi_scan",
        ParameterSpec::None,
        RECEIVER,
        false,
        false
    ),
];

pub fn presets(context: CommandContext) -> impl Iterator<Item = &'static Preset> {
    PRESETS
        .iter()
        .filter(move |preset| preset.supports(context))
}

// Authored task order, independent of translated labels and catalog filtering.
const GROUP_ORDER: &[&str] = &[
    "commands.group.information",
    "commands.group.sensor_calibration",
    "commands.group.connection",
    "commands.group.temperature_calibration",
    "commands.group.magnetometer",
    "commands.group.diagnostics",
    "commands.group.reset",
    "commands.group.management",
    "commands.group.statistics",
    "commands.group.radio",
    "commands.group.system",
];

const TRACKER_GROUP_ORDER: &[&str] = &[
    "commands.group.information",
    "commands.group.system",
    "commands.group.diagnostics",
    "commands.group.connection",
    "commands.group.sensor_calibration",
    "commands.group.magnetometer",
    "commands.group.temperature_calibration",
    "commands.group.reset",
    "commands.group.management",
    "commands.group.statistics",
    "commands.group.radio",
];

pub fn preset_groups(context: CommandContext) -> impl Iterator<Item = &'static str> {
    let group_order = match context {
        CommandContext::ForwardedTracker
        | CommandContext::Direct(crate::domain::DeviceRole::Tracker) => TRACKER_GROUP_ORDER,
        CommandContext::Direct(_) => GROUP_ORDER,
    };
    group_order
        .iter()
        .copied()
        .filter(move |group| presets(context).any(|preset| preset.group == *group))
}
