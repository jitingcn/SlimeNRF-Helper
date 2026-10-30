use super::super::{CommandSnapshot, CommandTone, command_tone};
use super::{direct, forwarded, get};
use crate::domain::DeviceRole;

#[test]
fn presentation_tone_does_not_weaken_confirmation_or_remote_authorization() {
    for id in ["mag-on", "mag-off"] {
        let preset = get(id);
        assert_eq!(preset.tone(), CommandTone::Normal);
        assert!(preset.dangerous);
        for route in [direct(DeviceRole::Tracker), forwarded()] {
            let snapshot = CommandSnapshot::capture(preset, "", route).unwrap();
            assert!(snapshot.dangerous());
            assert_eq!(command_tone(snapshot.command()), CommandTone::Normal);
            assert!(snapshot.authorize(route, false).is_err());
            assert!(snapshot.authorize(route, true).is_ok());
        }
    }
    for command in [
        "clear",
        "clearchannel",
        "remove",
        "reset all",
        "sens reset",
        "tcal clear",
        "tcal remove 2",
        "mag clear",
        "reset zro",
        "send all clear",
        "SEND 3 MAG CLEAR",
    ] {
        assert_eq!(command_tone(command), CommandTone::Destructive, "{command}");
    }
    for command in [
        "reboot",
        "dfu",
        "mag cal",
        "calibrate acc",
        "send 1 reset fusion",
        "channel 0",
        "send all channel 100",
        "tdma on",
        "send 3 tdma off",
        "test on",
        "send 3 test on 0",
        "send 3 test on 1000",
        "sens 0,0,0",
        "send 3 sens -1,2,0",
        "sens auto x",
        "send 3 sens auto z 100",
        "send 3 tcal heat start",
        "wom enter",
        "send 3 wom enter",
        "wom timeout 0",
        "send 3 wom timeout 35791",
    ] {
        assert_eq!(command_tone(command), CommandTone::Warning, "{command}");
    }
    for command in [
        "mag on",
        "mag off",
        "mag auto on",
        "pair",
        "info",
        "help",
        "send all mag off",
        "mag clearance",
        "clearance",
        "tcal status",
        "test off",
        "send 3 test off",
        "meow",
        "send 3 meow",
        "wom timeout",
        "send 3 wom timeout",
    ] {
        assert_eq!(command_tone(command), CommandTone::Normal, "{command}");
    }
}

#[test]
fn fusion_warning_retains_confirmation_for_direct_and_remote_commands() {
    for route in [direct(DeviceRole::Tracker), forwarded()] {
        let reset = CommandSnapshot::capture(get("reset-fusion"), "", route).unwrap();
        assert_eq!(command_tone(reset.command()), CommandTone::Warning);
        assert!(reset.dangerous());
        assert!(reset.authorize(route, false).is_err());
        assert!(reset.authorize(route, true).is_ok());
    }
    let reset = CommandSnapshot::capture(get("remote-reset"), "fusion", forwarded()).unwrap();
    assert_eq!(command_tone(reset.command()), CommandTone::Warning);
    assert!(reset.dangerous());
    assert!(reset.authorize(forwarded(), false).is_err());
    assert!(reset.authorize(forwarded(), true).is_ok());
    for invalid in ["sens", "all"] {
        assert!(reset.clone().with_parameter(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn wom_timeout_query_and_explicit_default_have_distinct_wire_text_and_tone() {
    for route in [direct(DeviceRole::Tracker), forwarded()] {
        let query = CommandSnapshot::capture(get("wom-timeout"), "", route).unwrap();
        assert_eq!(command_tone(query.command()), CommandTone::Normal);
        let restore = query.clone().with_parameter("0").unwrap();
        assert_eq!(restore.command(), format!("{} 0", query.command()));
        assert_eq!(command_tone(restore.command()), CommandTone::Warning);
        let query_again = restore.with_parameter("").unwrap();
        assert_eq!(query_again.command(), query.command());
        assert_eq!(command_tone(query_again.command()), CommandTone::Normal);
    }
}
