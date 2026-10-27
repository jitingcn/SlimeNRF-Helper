use crate::settings::{AccentColor, Appearance, Preferences};
use gpui_kit::{
    Hsla,
    component::{Theme, ThemeColor, ThemeMode},
    gpui::{App, rgb},
};

fn accent_palette(
    appearance: Appearance,
    accent: AccentColor,
) -> Option<(u32, u32, u32, u32, u32)> {
    match (appearance, accent) {
        (_, AccentColor::Neutral) => None,
        (Appearance::Light, AccentColor::Blue) => {
            Some((0x2563eb, 0x1d4ed8, 0x1e40af, 0xeff6ff, 0xbfdbfe))
        }
        (Appearance::Light, AccentColor::Green) => {
            Some((0x15803d, 0x166534, 0x14532d, 0xf0fdf4, 0xbbf7d0))
        }
        (Appearance::Light, AccentColor::Purple) => {
            Some((0x9333ea, 0x7e22ce, 0x6b21a8, 0xfaf5ff, 0xe9d5ff))
        }
        (Appearance::Light, AccentColor::Amber) => {
            Some((0xb45309, 0x92400e, 0x78350f, 0xfffbeb, 0xfde68a))
        }
        (Appearance::Light, AccentColor::Sakura) => {
            Some((0xad416e, 0x98365e, 0x842e52, 0xf7dde8, 0xecc5d7))
        }
        (Appearance::Dark, AccentColor::Blue) => {
            Some((0x60a5fa, 0x93c5fd, 0x3b82f6, 0x172b49, 0x1e40af))
        }
        (Appearance::Dark, AccentColor::Green) => {
            Some((0x4ade80, 0x86efac, 0x22c55e, 0x153524, 0x166534))
        }
        (Appearance::Dark, AccentColor::Purple) => {
            Some((0xc084fc, 0xd8b4fe, 0xa855f7, 0x322044, 0x6b21a8))
        }
        (Appearance::Dark, AccentColor::Amber) => {
            Some((0xfbbf24, 0xfcd34d, 0xf59e0b, 0x3c2c12, 0x92400e))
        }
        (Appearance::Dark, AccentColor::Sakura) => {
            Some((0xe9a2bf, 0xf2b8d0, 0xd68bab, 0x49303f, 0x5c3b4b))
        }
    }
}

/// Preview the primary color used when applying an appearance and accent.
pub(super) fn accent_primary(appearance: Appearance, accent: AccentColor) -> Hsla {
    accent_palette(appearance, accent).map_or_else(
        || match appearance {
            Appearance::Light => ThemeColor::light().primary,
            Appearance::Dark => ThemeColor::dark().primary,
        },
        |(primary, _, _, _, _)| rgb(primary).into(),
    )
}

pub(super) fn apply(preferences: &Preferences, cx: &mut App) {
    // The component setter updates rust-i18n's shared application locale as well.
    gpui_kit::component::set_locale(preferences.language.code());
    // Reload the complete mode baseline so switching away from Sakura also
    // resets its surface and semantic overrides, including within the same mode.
    // Theme::change/update refresh windows and sync the base terminal selection.
    let mode = match preferences.appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    let palette = accent_palette(preferences.appearance, preferences.accent);
    Theme::update(cx, |theme| {
        let colors = &mut theme.colors;
        // background = canvas/inset, sidebar = group body/navigation,
        // group_box = panel/card, muted = section/receiver header.
        match (preferences.appearance, preferences.accent) {
            (Appearance::Light, AccentColor::Sakura) => {
                colors.background = rgb(0xfffbfd).into();
                colors.sidebar = rgb(0xfcf4f8).into();
                colors.group_box = rgb(0xf6ebf1).into();
                colors.muted = rgb(0xeedfe7).into();
                colors.foreground = rgb(0x49323f).into();
                colors.muted_foreground = rgb(0x684858).into();
                colors.border = rgb(0xb69aaa).into();
                colors.input = rgb(0x9b7b8c).into();
                colors.secondary = colors.muted;
                colors.secondary_hover = rgb(0xe6d2dd).into();
                colors.secondary_active = rgb(0xdcc3cf).into();
            }
            (Appearance::Dark, AccentColor::Sakura) => {
                colors.background = rgb(0x17141a).into();
                colors.sidebar = rgb(0x201a23).into();
                colors.group_box = rgb(0x2c232d).into();
                colors.muted = rgb(0x352a36).into();
                colors.foreground = rgb(0xf5eaf0).into();
                colors.muted_foreground = rgb(0xc8b2c0).into();
                colors.border = rgb(0x685260).into();
                colors.input = rgb(0x806676).into();
                colors.secondary = colors.muted;
                colors.secondary_hover = rgb(0x423342).into();
                colors.secondary_active = rgb(0x503d4c).into();
            }
            (Appearance::Light, _) => {
                colors.background = rgb(0xffffff).into();
                colors.sidebar = rgb(0xf8f8f8).into();
                colors.group_box = rgb(0xf0f0f0).into();
                colors.muted = rgb(0xe8e8e8).into();
                colors.foreground = rgb(0x1e293b).into();
                colors.muted_foreground = rgb(0x526174).into();
                colors.border = rgb(0xb8b8b8).into();
                colors.input = rgb(0xa0a0a0).into();
                colors.secondary = rgb(0xe8e8e8).into();
                colors.secondary_hover = rgb(0xdcdcdc).into();
                colors.secondary_active = rgb(0xcfcfcf).into();
            }
            (Appearance::Dark, _) => {
                colors.background = rgb(0x11151c).into();
                colors.sidebar = rgb(0x191f29).into();
                colors.group_box = rgb(0x232c39).into();
                colors.muted = rgb(0x2b3747).into();
                colors.foreground = rgb(0xe7edf5).into();
                colors.muted_foreground = rgb(0xb6c2d2).into();
                colors.border = rgb(0x404d60).into();
                colors.input = rgb(0x63748b).into();
                colors.secondary = rgb(0x2b3747).into();
                colors.secondary_hover = rgb(0x39485b).into();
                colors.secondary_active = rgb(0x45566d).into();
            }
        }
        colors.group_box_foreground = colors.foreground;
        colors.sidebar_foreground = colors.foreground;
        colors.sidebar_border = colors.border;
        colors.popover = colors.group_box;
        colors.popover_foreground = colors.foreground;
        colors.secondary_foreground = colors.foreground;
        colors.button = colors.group_box;
        colors.button_hover = colors.secondary_hover;
        colors.button_active = colors.secondary_active;
        colors.button_foreground = colors.foreground;
        colors.button_secondary = colors.secondary;
        colors.button_secondary_hover = colors.secondary_hover;
        colors.button_secondary_active = colors.secondary_active;
        colors.button_secondary_foreground = colors.secondary_foreground;

        if let Some((primary, hover, active, subtle, selection)) = palette {
            // Light mode uses deep fills with light text; dark mode uses bright
            // fills with dark text. Subtle and selection fills suit the baseline text.
            let foreground = rgb(match (preferences.appearance, preferences.accent) {
                (Appearance::Light, AccentColor::Sakura) => 0xfffbfd,
                (Appearance::Dark, AccentColor::Sakura) => 0x291c25,
                (Appearance::Light, _) => 0xfafafa,
                (Appearance::Dark, _) => 0x0a0a0a,
            })
            .into();
            colors.primary = rgb(primary).into();
            colors.primary_hover = rgb(hover).into();
            colors.primary_active = rgb(active).into();
            colors.primary_foreground = foreground;
            colors.button_primary = colors.primary;
            colors.button_primary_hover = colors.primary_hover;
            colors.button_primary_active = colors.primary_active;
            colors.button_primary_foreground = foreground;
            colors.accent = rgb(subtle).into();
            colors.accent_foreground = colors.foreground;
            colors.ring = colors.primary;
            colors.caret = colors.primary;
            colors.link = colors.primary;
            colors.link_hover = colors.primary_hover;
            colors.link_active = colors.primary_active;
            colors.sidebar_primary = colors.primary;
            colors.sidebar_primary_foreground = foreground;
            colors.sidebar_accent = colors.accent;
            colors.sidebar_accent_foreground = colors.accent_foreground;
            colors.table_active = colors.accent;
            colors.table_active_border = colors.primary;
            colors.selection = rgb(selection).into();
            colors.drag_border = colors.primary;
            colors.drop_target = colors.accent;
            colors.progress_bar = colors.primary;
            colors.slider_bar = colors.primary;
        }

        // Tabs and lists follow the same workspace surfaces for every accent.
        colors.list = colors.group_box;
        colors.list_even = colors.sidebar;
        colors.list_head = colors.muted;
        colors.list_hover = colors.accent;
        colors.list_active = colors.accent;
        colors.list_active_border = colors.primary;
        colors.tab = colors.sidebar;
        colors.tab_foreground = colors.muted_foreground;
        colors.tab_active = colors.accent;
        colors.tab_active_foreground = colors.foreground;
        colors.tab_bar = colors.sidebar;
        colors.tab_bar_segmented = colors.muted;

        if preferences.accent == AccentColor::Sakura {
            colors.accordion = colors.group_box;
            colors.description_list_label = colors.muted;
            colors.description_list_label_foreground = colors.foreground;
            colors.table = colors.group_box;
            colors.table_even = colors.sidebar;
            colors.table_head = colors.muted;
            colors.table_head_foreground = colors.foreground;
            colors.table_foot = colors.muted;
            colors.table_foot_foreground = colors.foreground;
            colors.table_hover = colors.accent;
            colors.table_row_border = colors.border;
            colors.scrollbar = colors.sidebar;
            colors.scrollbar_thumb = colors.border;
            colors.scrollbar_thumb_hover = colors.input;
            colors.skeleton = colors.secondary_hover;
            colors.slider_thumb = colors.primary;
            colors.switch = colors.input;
            colors.switch_thumb = colors.background;
            colors.title_bar = colors.sidebar;
            colors.title_bar_border = colors.border;
            colors.status_bar = colors.sidebar;
            colors.status_bar_border = colors.border;
            colors.window_border = colors.border;
            // Keep statuses distinct from rose selection: sage, ochre, coral, blue.
            match preferences.appearance {
                Appearance::Light => {
                    colors.success = rgb(0x3e6a50).into();
                    colors.success_hover = rgb(0x345c44).into();
                    colors.success_active = rgb(0x294d37).into();
                    colors.warning = rgb(0x855b1c).into();
                    colors.warning_hover = rgb(0x714c16).into();
                    colors.warning_active = rgb(0x5e3e12).into();
                    colors.danger = rgb(0xa14332).into();
                    colors.danger_hover = rgb(0x8c392b).into();
                    colors.danger_active = rgb(0x773024).into();
                    colors.info = rgb(0x456986).into();
                    colors.info_hover = rgb(0x3a5b76).into();
                    colors.info_active = rgb(0x304d66).into();
                    colors.overlay = colors.foreground.opacity(0.28);
                }
                Appearance::Dark => {
                    colors.success = rgb(0xa2c4a6).into();
                    colors.success_hover = rgb(0xb6d5b8).into();
                    colors.success_active = rgb(0x8eb492).into();
                    colors.warning = rgb(0xe3bf7f).into();
                    colors.warning_hover = rgb(0xefd09a).into();
                    colors.warning_active = rgb(0xcda768).into();
                    colors.danger = rgb(0xf0a38e).into();
                    colors.danger_hover = rgb(0xf6b9a8).into();
                    colors.danger_active = rgb(0xde8e79).into();
                    colors.info = rgb(0xa1bed8).into();
                    colors.info_hover = rgb(0xb8cfe2).into();
                    colors.info_active = rgb(0x89a8c5).into();
                    colors.overlay = colors.background.opacity(0.65);
                }
            }
            colors.success_foreground = colors.primary_foreground;
            colors.warning_foreground = colors.primary_foreground;
            colors.danger_foreground = colors.primary_foreground;
            colors.info_foreground = colors.primary_foreground;
            colors.button_success = colors.success;
            colors.button_success_hover = colors.success_hover;
            colors.button_success_active = colors.success_active;
            colors.button_success_foreground = colors.success_foreground;
            colors.button_warning = colors.warning;
            colors.button_warning_hover = colors.warning_hover;
            colors.button_warning_active = colors.warning_active;
            colors.button_warning_foreground = colors.warning_foreground;
            colors.button_danger = colors.danger;
            colors.button_danger_hover = colors.danger_hover;
            colors.button_danger_active = colors.danger_active;
            colors.button_danger_foreground = colors.danger_foreground;
            colors.button_info = colors.info;
            colors.button_info_hover = colors.info_hover;
            colors.button_info_active = colors.info_active;
            colors.button_info_foreground = colors.info_foreground;
            colors.chart_1 = colors.primary;
            colors.chart_2 = colors.success;
            colors.chart_3 = colors.info;
            colors.chart_4 = colors.warning;
            colors.chart_5 = colors.muted_foreground;
            colors.chart_bullish = colors.success;
            colors.chart_bearish = colors.danger;
            colors.chart_grid = colors.border;
        }
    });
}
