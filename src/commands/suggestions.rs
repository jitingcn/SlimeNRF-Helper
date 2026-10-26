use super::{ParameterPreset, Preset};

impl Preset {
    /// Editable convenience choices; the pinned firmware parameter contract
    /// validates both selected suggestions and manually entered text.
    pub fn parameter_presets(&self) -> &'static [ParameterPreset] {
        match self.id {
            "tracker-channel" | "receiver-channel" => &[
                ParameterPreset {
                    label: None,
                    value: "0",
                },
                ParameterPreset {
                    label: None,
                    value: "1",
                },
                ParameterPreset {
                    label: None,
                    value: "2",
                },
                ParameterPreset {
                    label: None,
                    value: "3",
                },
                ParameterPreset {
                    label: None,
                    value: "4",
                },
                ParameterPreset {
                    label: None,
                    value: "5",
                },
                ParameterPreset {
                    label: None,
                    value: "6",
                },
                ParameterPreset {
                    label: None,
                    value: "7",
                },
                ParameterPreset {
                    label: None,
                    value: "8",
                },
                ParameterPreset {
                    label: None,
                    value: "9",
                },
                ParameterPreset {
                    label: None,
                    value: "10",
                },
                ParameterPreset {
                    label: None,
                    value: "11",
                },
                ParameterPreset {
                    label: None,
                    value: "12",
                },
                ParameterPreset {
                    label: None,
                    value: "13",
                },
                ParameterPreset {
                    label: None,
                    value: "14",
                },
                ParameterPreset {
                    label: None,
                    value: "15",
                },
                ParameterPreset {
                    label: None,
                    value: "16",
                },
                ParameterPreset {
                    label: None,
                    value: "17",
                },
                ParameterPreset {
                    label: None,
                    value: "18",
                },
                ParameterPreset {
                    label: None,
                    value: "19",
                },
                ParameterPreset {
                    label: None,
                    value: "20",
                },
                ParameterPreset {
                    label: None,
                    value: "21",
                },
                ParameterPreset {
                    label: None,
                    value: "22",
                },
                ParameterPreset {
                    label: None,
                    value: "23",
                },
                ParameterPreset {
                    label: None,
                    value: "24",
                },
                ParameterPreset {
                    label: None,
                    value: "25",
                },
                ParameterPreset {
                    label: None,
                    value: "26",
                },
                ParameterPreset {
                    label: None,
                    value: "27",
                },
                ParameterPreset {
                    label: None,
                    value: "28",
                },
                ParameterPreset {
                    label: None,
                    value: "29",
                },
                ParameterPreset {
                    label: None,
                    value: "30",
                },
                ParameterPreset {
                    label: None,
                    value: "31",
                },
                ParameterPreset {
                    label: None,
                    value: "32",
                },
                ParameterPreset {
                    label: None,
                    value: "33",
                },
                ParameterPreset {
                    label: None,
                    value: "34",
                },
                ParameterPreset {
                    label: None,
                    value: "35",
                },
                ParameterPreset {
                    label: None,
                    value: "36",
                },
                ParameterPreset {
                    label: None,
                    value: "37",
                },
                ParameterPreset {
                    label: None,
                    value: "38",
                },
                ParameterPreset {
                    label: None,
                    value: "39",
                },
                ParameterPreset {
                    label: None,
                    value: "40",
                },
                ParameterPreset {
                    label: None,
                    value: "41",
                },
                ParameterPreset {
                    label: None,
                    value: "42",
                },
                ParameterPreset {
                    label: None,
                    value: "43",
                },
                ParameterPreset {
                    label: None,
                    value: "44",
                },
                ParameterPreset {
                    label: None,
                    value: "45",
                },
                ParameterPreset {
                    label: None,
                    value: "46",
                },
                ParameterPreset {
                    label: None,
                    value: "47",
                },
                ParameterPreset {
                    label: None,
                    value: "48",
                },
                ParameterPreset {
                    label: None,
                    value: "49",
                },
                ParameterPreset {
                    label: None,
                    value: "50",
                },
                ParameterPreset {
                    label: None,
                    value: "51",
                },
                ParameterPreset {
                    label: None,
                    value: "52",
                },
                ParameterPreset {
                    label: None,
                    value: "53",
                },
                ParameterPreset {
                    label: None,
                    value: "54",
                },
                ParameterPreset {
                    label: None,
                    value: "55",
                },
                ParameterPreset {
                    label: None,
                    value: "56",
                },
                ParameterPreset {
                    label: None,
                    value: "57",
                },
                ParameterPreset {
                    label: None,
                    value: "58",
                },
                ParameterPreset {
                    label: None,
                    value: "59",
                },
                ParameterPreset {
                    label: None,
                    value: "60",
                },
                ParameterPreset {
                    label: None,
                    value: "61",
                },
                ParameterPreset {
                    label: None,
                    value: "62",
                },
                ParameterPreset {
                    label: None,
                    value: "63",
                },
                ParameterPreset {
                    label: None,
                    value: "64",
                },
                ParameterPreset {
                    label: None,
                    value: "65",
                },
                ParameterPreset {
                    label: None,
                    value: "66",
                },
                ParameterPreset {
                    label: None,
                    value: "67",
                },
                ParameterPreset {
                    label: None,
                    value: "68",
                },
                ParameterPreset {
                    label: None,
                    value: "69",
                },
                ParameterPreset {
                    label: None,
                    value: "70",
                },
                ParameterPreset {
                    label: None,
                    value: "71",
                },
                ParameterPreset {
                    label: None,
                    value: "72",
                },
                ParameterPreset {
                    label: None,
                    value: "73",
                },
                ParameterPreset {
                    label: None,
                    value: "74",
                },
                ParameterPreset {
                    label: None,
                    value: "75",
                },
                ParameterPreset {
                    label: None,
                    value: "76",
                },
                ParameterPreset {
                    label: None,
                    value: "77",
                },
                ParameterPreset {
                    label: None,
                    value: "78",
                },
                ParameterPreset {
                    label: None,
                    value: "79",
                },
                ParameterPreset {
                    label: None,
                    value: "80",
                },
                ParameterPreset {
                    label: None,
                    value: "81",
                },
                ParameterPreset {
                    label: None,
                    value: "82",
                },
                ParameterPreset {
                    label: None,
                    value: "83",
                },
                ParameterPreset {
                    label: None,
                    value: "84",
                },
                ParameterPreset {
                    label: None,
                    value: "85",
                },
                ParameterPreset {
                    label: None,
                    value: "86",
                },
                ParameterPreset {
                    label: None,
                    value: "87",
                },
                ParameterPreset {
                    label: None,
                    value: "88",
                },
                ParameterPreset {
                    label: None,
                    value: "89",
                },
                ParameterPreset {
                    label: None,
                    value: "90",
                },
                ParameterPreset {
                    label: None,
                    value: "91",
                },
                ParameterPreset {
                    label: None,
                    value: "92",
                },
                ParameterPreset {
                    label: None,
                    value: "93",
                },
                ParameterPreset {
                    label: None,
                    value: "94",
                },
                ParameterPreset {
                    label: None,
                    value: "95",
                },
                ParameterPreset {
                    label: None,
                    value: "96",
                },
                ParameterPreset {
                    label: None,
                    value: "97",
                },
                ParameterPreset {
                    label: None,
                    value: "98",
                },
                ParameterPreset {
                    label: None,
                    value: "99",
                },
                ParameterPreset {
                    label: None,
                    value: "100",
                },
            ],
            "sens-auto" => &[
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.axis_x"),
                    value: "x",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.axis_y"),
                    value: "y",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.axis_z"),
                    value: "z",
                },
            ],
            "test-on" => &[
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.built_in_default"),
                    value: "",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.tps_200"),
                    value: "200",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.tps_1000"),
                    value: "1000",
                },
            ],
            "wom-timeout" => &[
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.query_timeout"),
                    value: "",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.restore_default"),
                    value: "0",
                },
            ],
            "remote-reset" => &[
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.reset_gyroscope"),
                    value: "zro",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.reset_accelerometer"),
                    value: "acc",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.reset_battery"),
                    value: "bat",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.reset_magnetometer"),
                    value: "mag",
                },
                ParameterPreset {
                    label: Some("commands.parameter.suggestion.reset_temperature"),
                    value: "tcal",
                },
            ],
            _ => &[],
        }
    }
}
