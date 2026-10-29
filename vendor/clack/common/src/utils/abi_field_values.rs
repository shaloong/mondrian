//! Integer enum values projected into their actual CLAP field carriers.
//!
//! C enum tags keep their generated raw representation. Event IDs/flags and
//! signed note/log values use the explicit carriers prescribed by the headers.
//! Each narrowing is checked at compile time; this module defines no FFI type.

const fn checked_i32_to_u16(value: i32) -> u16 {
    assert!(value >= 0);
    assert!(value <= u16::MAX as i32);
    // Parameter bounds above prove every accepted value fits.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap
    )]
    {
        value as u16
    }
}

const fn checked_i32_to_u32(value: i32) -> u32 {
    assert!(value >= 0);
    // Preserve the nonnegative integer bits using const APIs available at our MSRV.
    u32::from_ne_bytes(value.to_ne_bytes())
}

const fn checked_i32_to_u64(value: i32) -> u64 {
    assert!(value >= 0);
    // Parameter bounds above prove every accepted value fits.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap
    )]
    {
        value as u64
    }
}

const fn checked_i32_to_usize(value: i32) -> usize {
    assert!(value >= 0);
    const { assert!(usize::BITS >= u32::BITS) };
    // Parameter bounds above prove every accepted value fits.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap
    )]
    {
        value as usize
    }
}

/// Official `CLAP_AMBISONIC_NORMALIZATION_MAXN` represented as its prescribed u32 field.
pub const CLAP_AMBISONIC_NORMALIZATION_MAXN: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AMBISONIC_NORMALIZATION_MAXN);

/// Official `CLAP_AMBISONIC_NORMALIZATION_N2D` represented as its prescribed u32 field.
pub const CLAP_AMBISONIC_NORMALIZATION_N2D: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AMBISONIC_NORMALIZATION_N2D);

/// Official `CLAP_AMBISONIC_NORMALIZATION_N3D` represented as its prescribed u32 field.
pub const CLAP_AMBISONIC_NORMALIZATION_N3D: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AMBISONIC_NORMALIZATION_N3D);

/// Official `CLAP_AMBISONIC_NORMALIZATION_SN2D` represented as its prescribed u32 field.
pub const CLAP_AMBISONIC_NORMALIZATION_SN2D: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AMBISONIC_NORMALIZATION_SN2D);

/// Official `CLAP_AMBISONIC_NORMALIZATION_SN3D` represented as its prescribed u32 field.
pub const CLAP_AMBISONIC_NORMALIZATION_SN3D: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AMBISONIC_NORMALIZATION_SN3D);

/// Official `CLAP_AMBISONIC_ORDERING_ACN` represented as its prescribed u32 field.
pub const CLAP_AMBISONIC_ORDERING_ACN: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AMBISONIC_ORDERING_ACN);

/// Official `CLAP_AMBISONIC_ORDERING_FUMA` represented as its prescribed u32 field.
pub const CLAP_AMBISONIC_ORDERING_FUMA: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AMBISONIC_ORDERING_FUMA);

/// Official `CLAP_AUDIO_PORTS_RESCAN_CHANNEL_COUNT` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORTS_RESCAN_CHANNEL_COUNT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORTS_RESCAN_CHANNEL_COUNT);

/// Official `CLAP_AUDIO_PORTS_RESCAN_FLAGS` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORTS_RESCAN_FLAGS: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORTS_RESCAN_FLAGS);

/// Official `CLAP_AUDIO_PORTS_RESCAN_IN_PLACE_PAIR` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORTS_RESCAN_IN_PLACE_PAIR: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORTS_RESCAN_IN_PLACE_PAIR);

/// Official `CLAP_AUDIO_PORTS_RESCAN_LIST` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORTS_RESCAN_LIST: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORTS_RESCAN_LIST);

/// Official `CLAP_AUDIO_PORTS_RESCAN_NAMES` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORTS_RESCAN_NAMES: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORTS_RESCAN_NAMES);

/// Official `CLAP_AUDIO_PORTS_RESCAN_PORT_TYPE` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORTS_RESCAN_PORT_TYPE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORTS_RESCAN_PORT_TYPE);

/// Official `CLAP_AUDIO_PORT_IS_MAIN` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORT_IS_MAIN: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORT_IS_MAIN);

/// Official `CLAP_AUDIO_PORT_PREFERS_64BITS` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORT_PREFERS_64BITS: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORT_PREFERS_64BITS);

/// Official `CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORT_REQUIRES_COMMON_SAMPLE_SIZE);

/// Official `CLAP_AUDIO_PORT_SUPPORTS_64BITS` represented as its prescribed u32 field.
pub const CLAP_AUDIO_PORT_SUPPORTS_64BITS: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_AUDIO_PORT_SUPPORTS_64BITS);

/// Official `CLAP_CONTEXT_MENU_ITEM_BEGIN_SUBMENU` represented as its prescribed u32 field.
pub const CLAP_CONTEXT_MENU_ITEM_BEGIN_SUBMENU: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_CONTEXT_MENU_ITEM_BEGIN_SUBMENU);

/// Official `CLAP_CONTEXT_MENU_ITEM_CHECK_ENTRY` represented as its prescribed u32 field.
pub const CLAP_CONTEXT_MENU_ITEM_CHECK_ENTRY: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_CONTEXT_MENU_ITEM_CHECK_ENTRY);

/// Official `CLAP_CONTEXT_MENU_ITEM_END_SUBMENU` represented as its prescribed u32 field.
pub const CLAP_CONTEXT_MENU_ITEM_END_SUBMENU: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_CONTEXT_MENU_ITEM_END_SUBMENU);

/// Official `CLAP_CONTEXT_MENU_ITEM_ENTRY` represented as its prescribed u32 field.
pub const CLAP_CONTEXT_MENU_ITEM_ENTRY: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_CONTEXT_MENU_ITEM_ENTRY);

/// Official `CLAP_CONTEXT_MENU_ITEM_SEPARATOR` represented as its prescribed u32 field.
pub const CLAP_CONTEXT_MENU_ITEM_SEPARATOR: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_CONTEXT_MENU_ITEM_SEPARATOR);

/// Official `CLAP_CONTEXT_MENU_ITEM_TITLE` represented as its prescribed u32 field.
pub const CLAP_CONTEXT_MENU_ITEM_TITLE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_CONTEXT_MENU_ITEM_TITLE);

/// Official `CLAP_CONTEXT_MENU_TARGET_KIND_GLOBAL` represented as its prescribed u32 field.
pub const CLAP_CONTEXT_MENU_TARGET_KIND_GLOBAL: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_CONTEXT_MENU_TARGET_KIND_GLOBAL);

/// Official `CLAP_CONTEXT_MENU_TARGET_KIND_PARAM` represented as its prescribed u32 field.
pub const CLAP_CONTEXT_MENU_TARGET_KIND_PARAM: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_CONTEXT_MENU_TARGET_KIND_PARAM);

/// Official `CLAP_EVENT_DONT_RECORD` represented as its prescribed u32 field.
pub const CLAP_EVENT_DONT_RECORD: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_EVENT_DONT_RECORD);

/// Official `CLAP_EVENT_IS_LIVE` represented as its prescribed u32 field.
pub const CLAP_EVENT_IS_LIVE: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_EVENT_IS_LIVE);

/// Official `CLAP_EVENT_MIDI` represented as its prescribed u16 field.
pub const CLAP_EVENT_MIDI: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_MIDI);

/// Official `CLAP_EVENT_MIDI2` represented as its prescribed u16 field.
pub const CLAP_EVENT_MIDI2: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_MIDI2);

/// Official `CLAP_EVENT_MIDI_SYSEX` represented as its prescribed u16 field.
pub const CLAP_EVENT_MIDI_SYSEX: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_MIDI_SYSEX);

/// Official `CLAP_EVENT_NOTE_CHOKE` represented as its prescribed u16 field.
pub const CLAP_EVENT_NOTE_CHOKE: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_NOTE_CHOKE);

/// Official `CLAP_EVENT_NOTE_END` represented as its prescribed u16 field.
pub const CLAP_EVENT_NOTE_END: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_NOTE_END);

/// Official `CLAP_EVENT_NOTE_EXPRESSION` represented as its prescribed u16 field.
pub const CLAP_EVENT_NOTE_EXPRESSION: u16 =
    checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_NOTE_EXPRESSION);

/// Official `CLAP_EVENT_NOTE_OFF` represented as its prescribed u16 field.
pub const CLAP_EVENT_NOTE_OFF: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_NOTE_OFF);

/// Official `CLAP_EVENT_NOTE_ON` represented as its prescribed u16 field.
pub const CLAP_EVENT_NOTE_ON: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_NOTE_ON);

/// Official `CLAP_EVENT_PARAM_GESTURE_BEGIN` represented as its prescribed u16 field.
pub const CLAP_EVENT_PARAM_GESTURE_BEGIN: u16 =
    checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_PARAM_GESTURE_BEGIN);

/// Official `CLAP_EVENT_PARAM_GESTURE_END` represented as its prescribed u16 field.
pub const CLAP_EVENT_PARAM_GESTURE_END: u16 =
    checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_PARAM_GESTURE_END);

/// Official `CLAP_EVENT_PARAM_MOD` represented as its prescribed u16 field.
pub const CLAP_EVENT_PARAM_MOD: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_PARAM_MOD);

/// Official `CLAP_EVENT_PARAM_VALUE` represented as its prescribed u16 field.
pub const CLAP_EVENT_PARAM_VALUE: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_PARAM_VALUE);

/// Official `CLAP_EVENT_TRANSPORT` represented as its prescribed u16 field.
pub const CLAP_EVENT_TRANSPORT: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_TRANSPORT);

/// Official `CLAP_EVENT_TRIGGER` represented as its prescribed u16 field.
pub const CLAP_EVENT_TRIGGER: u16 = checked_i32_to_u16(clap_sys::raw::CLAP_EVENT_TRIGGER);

/// Official `CLAP_NAME_SIZE` represented as its prescribed Rust array extent.
pub const CLAP_NAME_SIZE: usize = checked_i32_to_usize(clap_sys::raw::CLAP_NAME_SIZE);

/// Official `CLAP_NOTE_DIALECT_CLAP` represented as its prescribed u32 field.
pub const CLAP_NOTE_DIALECT_CLAP: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_NOTE_DIALECT_CLAP);

/// Official `CLAP_NOTE_DIALECT_MIDI` represented as its prescribed u32 field.
pub const CLAP_NOTE_DIALECT_MIDI: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_NOTE_DIALECT_MIDI);

/// Official `CLAP_NOTE_DIALECT_MIDI2` represented as its prescribed u32 field.
pub const CLAP_NOTE_DIALECT_MIDI2: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_NOTE_DIALECT_MIDI2);

/// Official `CLAP_NOTE_DIALECT_MIDI_MPE` represented as its prescribed u32 field.
pub const CLAP_NOTE_DIALECT_MIDI_MPE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_NOTE_DIALECT_MIDI_MPE);

/// Official `CLAP_NOTE_PORTS_RESCAN_ALL` represented as its prescribed u32 field.
pub const CLAP_NOTE_PORTS_RESCAN_ALL: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_NOTE_PORTS_RESCAN_ALL);

/// Official `CLAP_NOTE_PORTS_RESCAN_NAMES` represented as its prescribed u32 field.
pub const CLAP_NOTE_PORTS_RESCAN_NAMES: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_NOTE_PORTS_RESCAN_NAMES);

/// Official `CLAP_PARAM_CLEAR_ALL` represented as its prescribed u32 field.
pub const CLAP_PARAM_CLEAR_ALL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_CLEAR_ALL);

/// Official `CLAP_PARAM_CLEAR_AUTOMATIONS` represented as its prescribed u32 field.
pub const CLAP_PARAM_CLEAR_AUTOMATIONS: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_CLEAR_AUTOMATIONS);

/// Official `CLAP_PARAM_CLEAR_MODULATIONS` represented as its prescribed u32 field.
pub const CLAP_PARAM_CLEAR_MODULATIONS: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_CLEAR_MODULATIONS);

/// Official `CLAP_PARAM_INDICATION_AUTOMATION_NONE` represented as its prescribed u32 field.
pub const CLAP_PARAM_INDICATION_AUTOMATION_NONE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_INDICATION_AUTOMATION_NONE);

/// Official `CLAP_PARAM_INDICATION_AUTOMATION_OVERRIDING` represented as its prescribed u32 field.
pub const CLAP_PARAM_INDICATION_AUTOMATION_OVERRIDING: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_INDICATION_AUTOMATION_OVERRIDING);

/// Official `CLAP_PARAM_INDICATION_AUTOMATION_PLAYING` represented as its prescribed u32 field.
pub const CLAP_PARAM_INDICATION_AUTOMATION_PLAYING: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_INDICATION_AUTOMATION_PLAYING);

/// Official `CLAP_PARAM_INDICATION_AUTOMATION_PRESENT` represented as its prescribed u32 field.
pub const CLAP_PARAM_INDICATION_AUTOMATION_PRESENT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_INDICATION_AUTOMATION_PRESENT);

/// Official `CLAP_PARAM_INDICATION_AUTOMATION_RECORDING` represented as its prescribed u32 field.
pub const CLAP_PARAM_INDICATION_AUTOMATION_RECORDING: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_INDICATION_AUTOMATION_RECORDING);

/// Official `CLAP_PARAM_IS_AUTOMATABLE` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_AUTOMATABLE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_AUTOMATABLE);

/// Official `CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL);

/// Official `CLAP_PARAM_IS_AUTOMATABLE_PER_KEY` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_AUTOMATABLE_PER_KEY: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_AUTOMATABLE_PER_KEY);

/// Official `CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID);

/// Official `CLAP_PARAM_IS_AUTOMATABLE_PER_PORT` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_AUTOMATABLE_PER_PORT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_AUTOMATABLE_PER_PORT);

/// Official `CLAP_PARAM_IS_BYPASS` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_BYPASS: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_BYPASS);

/// Official `CLAP_PARAM_IS_ENUM` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_ENUM: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_ENUM);

/// Official `CLAP_PARAM_IS_HIDDEN` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_HIDDEN: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_HIDDEN);

/// Official `CLAP_PARAM_IS_MODULATABLE` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_MODULATABLE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_MODULATABLE);

/// Official `CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL);

/// Official `CLAP_PARAM_IS_MODULATABLE_PER_KEY` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_MODULATABLE_PER_KEY: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_MODULATABLE_PER_KEY);

/// Official `CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID);

/// Official `CLAP_PARAM_IS_MODULATABLE_PER_PORT` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_MODULATABLE_PER_PORT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_MODULATABLE_PER_PORT);

/// Official `CLAP_PARAM_IS_PERIODIC` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_PERIODIC: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_PERIODIC);

/// Official `CLAP_PARAM_IS_READONLY` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_READONLY: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_READONLY);

/// Official `CLAP_PARAM_IS_STEPPED` represented as its prescribed u32 field.
pub const CLAP_PARAM_IS_STEPPED: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_IS_STEPPED);

/// Official `CLAP_PARAM_REQUIRES_PROCESS` represented as its prescribed u32 field.
pub const CLAP_PARAM_REQUIRES_PROCESS: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_REQUIRES_PROCESS);

/// Official `CLAP_PARAM_RESCAN_ALL` represented as its prescribed u32 field.
pub const CLAP_PARAM_RESCAN_ALL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_RESCAN_ALL);

/// Official `CLAP_PARAM_RESCAN_INFO` represented as its prescribed u32 field.
pub const CLAP_PARAM_RESCAN_INFO: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_RESCAN_INFO);

/// Official `CLAP_PARAM_RESCAN_TEXT` represented as its prescribed u32 field.
pub const CLAP_PARAM_RESCAN_TEXT: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_RESCAN_TEXT);

/// Official `CLAP_PARAM_RESCAN_VALUES` represented as its prescribed u32 field.
pub const CLAP_PARAM_RESCAN_VALUES: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PARAM_RESCAN_VALUES);

/// Official `CLAP_PATH_SIZE` represented as its prescribed Rust array extent.
pub const CLAP_PATH_SIZE: usize = checked_i32_to_usize(clap_sys::raw::CLAP_PATH_SIZE);

/// Official `CLAP_POSIX_FD_ERROR` represented as its prescribed u32 field.
pub const CLAP_POSIX_FD_ERROR: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_POSIX_FD_ERROR);

/// Official `CLAP_POSIX_FD_READ` represented as its prescribed u32 field.
pub const CLAP_POSIX_FD_READ: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_POSIX_FD_READ);

/// Official `CLAP_POSIX_FD_WRITE` represented as its prescribed u32 field.
pub const CLAP_POSIX_FD_WRITE: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_POSIX_FD_WRITE);

/// Official `CLAP_PRESET_DISCOVERY_IS_DEMO_CONTENT` represented as its prescribed u32 field.
pub const CLAP_PRESET_DISCOVERY_IS_DEMO_CONTENT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PRESET_DISCOVERY_IS_DEMO_CONTENT);

/// Official `CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT` represented as its prescribed u32 field.
pub const CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT);

/// Official `CLAP_PRESET_DISCOVERY_IS_FAVORITE` represented as its prescribed u32 field.
pub const CLAP_PRESET_DISCOVERY_IS_FAVORITE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PRESET_DISCOVERY_IS_FAVORITE);

/// Official `CLAP_PRESET_DISCOVERY_IS_USER_CONTENT` represented as its prescribed u32 field.
pub const CLAP_PRESET_DISCOVERY_IS_USER_CONTENT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PRESET_DISCOVERY_IS_USER_CONTENT);

/// Official `CLAP_PRESET_DISCOVERY_LOCATION_FILE` represented as its prescribed u32 field.
pub const CLAP_PRESET_DISCOVERY_LOCATION_FILE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PRESET_DISCOVERY_LOCATION_FILE);

/// Official `CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN` represented as its prescribed u32 field.
pub const CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN);

/// Official `CLAP_REMOTE_CONTROLS_COUNT` represented as its prescribed Rust array extent.
pub const CLAP_REMOTE_CONTROLS_COUNT: usize =
    checked_i32_to_usize(clap_sys::raw::CLAP_REMOTE_CONTROLS_COUNT);

/// Official `CLAP_STATE_CONTEXT_FOR_DUPLICATE` represented as its prescribed u32 field.
pub const CLAP_STATE_CONTEXT_FOR_DUPLICATE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_STATE_CONTEXT_FOR_DUPLICATE);

/// Official `CLAP_STATE_CONTEXT_FOR_PRESET` represented as its prescribed u32 field.
pub const CLAP_STATE_CONTEXT_FOR_PRESET: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_STATE_CONTEXT_FOR_PRESET);

/// Official `CLAP_STATE_CONTEXT_FOR_PROJECT` represented as its prescribed u32 field.
pub const CLAP_STATE_CONTEXT_FOR_PROJECT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_STATE_CONTEXT_FOR_PROJECT);

/// Official `CLAP_SURROUND_BC` represented as its prescribed u32 field.
pub const CLAP_SURROUND_BC: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_BC);

/// Official `CLAP_SURROUND_BL` represented as its prescribed u32 field.
pub const CLAP_SURROUND_BL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_BL);

/// Official `CLAP_SURROUND_BR` represented as its prescribed u32 field.
pub const CLAP_SURROUND_BR: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_BR);

/// Official `CLAP_SURROUND_FC` represented as its prescribed u32 field.
pub const CLAP_SURROUND_FC: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_FC);

/// Official `CLAP_SURROUND_FL` represented as its prescribed u32 field.
pub const CLAP_SURROUND_FL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_FL);

/// Official `CLAP_SURROUND_FLC` represented as its prescribed u32 field.
pub const CLAP_SURROUND_FLC: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_FLC);

/// Official `CLAP_SURROUND_FR` represented as its prescribed u32 field.
pub const CLAP_SURROUND_FR: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_FR);

/// Official `CLAP_SURROUND_FRC` represented as its prescribed u32 field.
pub const CLAP_SURROUND_FRC: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_FRC);

/// Official `CLAP_SURROUND_LFE` represented as its prescribed u32 field.
pub const CLAP_SURROUND_LFE: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_LFE);

/// Official `CLAP_SURROUND_SL` represented as its prescribed u32 field.
pub const CLAP_SURROUND_SL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_SL);

/// Official `CLAP_SURROUND_SR` represented as its prescribed u32 field.
pub const CLAP_SURROUND_SR: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_SR);

/// Official `CLAP_SURROUND_TBC` represented as its prescribed u32 field.
pub const CLAP_SURROUND_TBC: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_TBC);

/// Official `CLAP_SURROUND_TBL` represented as its prescribed u32 field.
pub const CLAP_SURROUND_TBL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_TBL);

/// Official `CLAP_SURROUND_TBR` represented as its prescribed u32 field.
pub const CLAP_SURROUND_TBR: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_TBR);

/// Official `CLAP_SURROUND_TC` represented as its prescribed u32 field.
pub const CLAP_SURROUND_TC: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_TC);

/// Official `CLAP_SURROUND_TFC` represented as its prescribed u32 field.
pub const CLAP_SURROUND_TFC: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_TFC);

/// Official `CLAP_SURROUND_TFL` represented as its prescribed u32 field.
pub const CLAP_SURROUND_TFL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_TFL);

/// Official `CLAP_SURROUND_TFR` represented as its prescribed u32 field.
pub const CLAP_SURROUND_TFR: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_SURROUND_TFR);

/// Official `CLAP_TRACK_INFO_HAS_AUDIO_CHANNEL` represented as its prescribed u64 field.
pub const CLAP_TRACK_INFO_HAS_AUDIO_CHANNEL: u64 =
    checked_i32_to_u64(clap_sys::raw::CLAP_TRACK_INFO_HAS_AUDIO_CHANNEL);

/// Official `CLAP_TRACK_INFO_HAS_TRACK_COLOR` represented as its prescribed u64 field.
pub const CLAP_TRACK_INFO_HAS_TRACK_COLOR: u64 =
    checked_i32_to_u64(clap_sys::raw::CLAP_TRACK_INFO_HAS_TRACK_COLOR);

/// Official `CLAP_TRACK_INFO_HAS_TRACK_NAME` represented as its prescribed u64 field.
pub const CLAP_TRACK_INFO_HAS_TRACK_NAME: u64 =
    checked_i32_to_u64(clap_sys::raw::CLAP_TRACK_INFO_HAS_TRACK_NAME);

/// Official `CLAP_TRACK_INFO_IS_FOR_BUS` represented as its prescribed u64 field.
pub const CLAP_TRACK_INFO_IS_FOR_BUS: u64 =
    checked_i32_to_u64(clap_sys::raw::CLAP_TRACK_INFO_IS_FOR_BUS);

/// Official `CLAP_TRACK_INFO_IS_FOR_MASTER` represented as its prescribed u64 field.
pub const CLAP_TRACK_INFO_IS_FOR_MASTER: u64 =
    checked_i32_to_u64(clap_sys::raw::CLAP_TRACK_INFO_IS_FOR_MASTER);

/// Official `CLAP_TRACK_INFO_IS_FOR_RETURN_TRACK` represented as its prescribed u64 field.
pub const CLAP_TRACK_INFO_IS_FOR_RETURN_TRACK: u64 =
    checked_i32_to_u64(clap_sys::raw::CLAP_TRACK_INFO_IS_FOR_RETURN_TRACK);

/// Official `CLAP_TRANSPORT_HAS_BEATS_TIMELINE` represented as its prescribed u32 field.
pub const CLAP_TRANSPORT_HAS_BEATS_TIMELINE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRANSPORT_HAS_BEATS_TIMELINE);

/// Official `CLAP_TRANSPORT_HAS_SECONDS_TIMELINE` represented as its prescribed u32 field.
pub const CLAP_TRANSPORT_HAS_SECONDS_TIMELINE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRANSPORT_HAS_SECONDS_TIMELINE);

/// Official `CLAP_TRANSPORT_HAS_TEMPO` represented as its prescribed u32 field.
pub const CLAP_TRANSPORT_HAS_TEMPO: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRANSPORT_HAS_TEMPO);

/// Official `CLAP_TRANSPORT_HAS_TIME_SIGNATURE` represented as its prescribed u32 field.
pub const CLAP_TRANSPORT_HAS_TIME_SIGNATURE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRANSPORT_HAS_TIME_SIGNATURE);

/// Official `CLAP_TRANSPORT_IS_LOOP_ACTIVE` represented as its prescribed u32 field.
pub const CLAP_TRANSPORT_IS_LOOP_ACTIVE: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRANSPORT_IS_LOOP_ACTIVE);

/// Official `CLAP_TRANSPORT_IS_PLAYING` represented as its prescribed u32 field.
pub const CLAP_TRANSPORT_IS_PLAYING: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRANSPORT_IS_PLAYING);

/// Official `CLAP_TRANSPORT_IS_RECORDING` represented as its prescribed u32 field.
pub const CLAP_TRANSPORT_IS_RECORDING: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRANSPORT_IS_RECORDING);

/// Official `CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL` represented as its prescribed u32 field.
pub const CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRANSPORT_IS_WITHIN_PRE_ROLL);

/// Official `CLAP_TRIGGER_CLEAR_ALL` represented as its prescribed u32 field.
pub const CLAP_TRIGGER_CLEAR_ALL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_TRIGGER_CLEAR_ALL);

/// Official `CLAP_TRIGGER_CLEAR_AUTOMATIONS` represented as its prescribed u32 field.
pub const CLAP_TRIGGER_CLEAR_AUTOMATIONS: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRIGGER_CLEAR_AUTOMATIONS);

/// Official `CLAP_TRIGGER_IS_AUTOMATABLE_PER_CHANNEL` represented as its prescribed u32 field.
pub const CLAP_TRIGGER_IS_AUTOMATABLE_PER_CHANNEL: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRIGGER_IS_AUTOMATABLE_PER_CHANNEL);

/// Official `CLAP_TRIGGER_IS_AUTOMATABLE_PER_KEY` represented as its prescribed u32 field.
pub const CLAP_TRIGGER_IS_AUTOMATABLE_PER_KEY: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRIGGER_IS_AUTOMATABLE_PER_KEY);

/// Official `CLAP_TRIGGER_IS_AUTOMATABLE_PER_NOTE_ID` represented as its prescribed u32 field.
pub const CLAP_TRIGGER_IS_AUTOMATABLE_PER_NOTE_ID: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRIGGER_IS_AUTOMATABLE_PER_NOTE_ID);

/// Official `CLAP_TRIGGER_IS_AUTOMATABLE_PER_PORT` represented as its prescribed u32 field.
pub const CLAP_TRIGGER_IS_AUTOMATABLE_PER_PORT: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRIGGER_IS_AUTOMATABLE_PER_PORT);

/// Official `CLAP_TRIGGER_RESCAN_ALL` represented as its prescribed u32 field.
pub const CLAP_TRIGGER_RESCAN_ALL: u32 = checked_i32_to_u32(clap_sys::raw::CLAP_TRIGGER_RESCAN_ALL);

/// Official `CLAP_TRIGGER_RESCAN_INFO` represented as its prescribed u32 field.
pub const CLAP_TRIGGER_RESCAN_INFO: u32 =
    checked_i32_to_u32(clap_sys::raw::CLAP_TRIGGER_RESCAN_INFO);

/// Official `CLAP_VOICE_INFO_SUPPORTS_OVERLAPPING_NOTES` represented as its prescribed u64 field.
pub const CLAP_VOICE_INFO_SUPPORTS_OVERLAPPING_NOTES: u64 =
    checked_i32_to_u64(clap_sys::raw::CLAP_VOICE_INFO_SUPPORTS_OVERLAPPING_NOTES);
