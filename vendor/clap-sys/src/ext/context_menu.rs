//! Reexports of declarations from official `clap/ext/context-menu.h`.
pub use crate::raw::{
    clap_context_menu_builder, clap_context_menu_builder_t, clap_context_menu_check_entry,
    clap_context_menu_check_entry_t, clap_context_menu_entry, clap_context_menu_entry_t,
    clap_context_menu_item_kind_t, clap_context_menu_item_title, clap_context_menu_item_title_t,
    clap_context_menu_submenu, clap_context_menu_submenu_t, clap_context_menu_target,
    clap_context_menu_target_t, clap_host_context_menu, clap_host_context_menu_t, clap_host_t,
    clap_id, clap_plugin_context_menu, clap_plugin_context_menu_t, clap_plugin_t,
    CLAP_CONTEXT_MENU_ITEM_BEGIN_SUBMENU, CLAP_CONTEXT_MENU_ITEM_CHECK_ENTRY,
    CLAP_CONTEXT_MENU_ITEM_END_SUBMENU, CLAP_CONTEXT_MENU_ITEM_ENTRY,
    CLAP_CONTEXT_MENU_ITEM_SEPARATOR, CLAP_CONTEXT_MENU_ITEM_TITLE,
    CLAP_CONTEXT_MENU_TARGET_KIND_GLOBAL, CLAP_CONTEXT_MENU_TARGET_KIND_PARAM,
    CLAP_EXT_CONTEXT_MENU, CLAP_EXT_CONTEXT_MENU_COMPAT,
};
