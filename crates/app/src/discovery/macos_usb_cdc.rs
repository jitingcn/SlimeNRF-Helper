#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ControlInterface {
    pub(super) number: u8,
    pub(super) string_index: u8,
}

/// Resolve the ACM control descriptor that names this exact USB data interface.
/// Darwin attaches the serial driver to the data interface, which need not be
/// adjacent to its control interface and usually has no interface string.
pub(super) fn control_interface(config: &[u8], data_interface: u8) -> Option<ControlInterface> {
    let &[9, 2, total_low, total_high, ..] = config.get(..9)? else {
        return None;
    };
    if usize::from(u16::from_le_bytes([total_low, total_high])) != config.len() {
        return None;
    }

    let mut descriptors = &config[9..];
    let mut current_control = None;
    let mut control = None;
    let mut data_found = false;
    while let Some(&length) = descriptors.first() {
        let (descriptor, remaining) = descriptors.split_at_checked(usize::from(length))?;
        descriptors = remaining;
        match descriptor {
            [
                _,
                4,
                number,
                alternate,
                _,
                class,
                subclass,
                _,
                string_index,
                ..,
            ] => {
                if *number == data_interface && *alternate == 0 {
                    if data_found || *class != 0x0a {
                        return None;
                    }
                    data_found = true;
                }
                current_control = (*alternate == 0 && *class == 0x02 && *subclass == 0x02)
                    .then_some(ControlInterface {
                        number: *number,
                        string_index: *string_index,
                    });
            }
            [_, 0x24, 0x06, master, members @ ..]
                if current_control.is_some() && members.contains(&data_interface) =>
            {
                let candidate = current_control?;
                if *master != candidate.number || members.len() != 1 || control.is_some() {
                    return None;
                }
                control = Some(candidate);
            }
            [_, 4, ..] | [] | [_] => return None,
            _ => {}
        }
    }
    control.filter(|_| data_found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration(descriptors: &[&[u8]]) -> Vec<u8> {
        let length = u16::try_from(9 + descriptors.iter().map(|d| d.len()).sum::<usize>())
            .unwrap()
            .to_le_bytes();
        let mut config = vec![9, 2, length[0], length[1], 0, 1, 0, 0x80, 50];
        config.extend(descriptors.iter().copied().flatten());
        config
    }

    #[test]
    fn cdc_union_links_distinct_mac_serial_functions_without_using_adjacency() {
        let config = configuration(&[
            &[9, 4, 0, 0, 1, 2, 2, 1, 0],
            &[5, 0x24, 0x06, 0, 4],
            &[9, 4, 2, 0, 1, 2, 2, 1, 6],
            &[5, 0x24, 0x06, 2, 3],
            &[9, 4, 3, 0, 2, 0x0a, 0, 0, 0],
            &[9, 4, 4, 0, 2, 0x0a, 0, 0, 0],
        ]);
        assert_eq!(
            control_interface(&config, 4),
            Some(ControlInterface {
                number: 0,
                string_index: 0,
            })
        );
        assert_eq!(
            control_interface(&config, 3),
            Some(ControlInterface {
                number: 2,
                string_index: 6,
            })
        );
        assert_eq!(control_interface(&config, 5), None);
    }

    #[test]
    fn cdc_union_rejects_unproved_or_conflicting_bindings() {
        let control = [9, 4, 0, 0, 1, 2, 2, 1, 6];
        let data = [9, 4, 1, 0, 2, 0x0a, 0, 0, 0];
        let union = [5, 0x24, 0x06, 0, 1];
        let duplicate = [9, 4, 2, 0, 1, 2, 2, 1, 0];
        let cases: &[&[&[u8]]] = &[
            &[&control, &data],
            &[&control, &union],
            &[&control, &[5, 0x24, 0x06, 3, 1], &data],
            &[&control, &union, &duplicate, &[5, 0x24, 0x06, 2, 1], &data],
            &[&control, &union, &data, &[0, 0]],
        ];
        for descriptors in cases {
            assert_eq!(control_interface(&configuration(descriptors), 1), None);
        }
        let mut truncated = configuration(&[&control, &union, &data]);
        truncated.pop();
        assert_eq!(control_interface(&truncated, 1), None);
    }
}
