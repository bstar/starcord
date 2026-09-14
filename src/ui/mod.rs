//! The terminal interface.
//!
//! Shaped the way STAR/AMP's is, and for the same reasons. The window is one
//! vertical column of modules in the order you drill through them, each
//! folding to a single line when you are done with it; [`keymap`] compiles
//! with no reference to [`app`]; a module is a widget over a view struct and
//! owns none of what it draws; [`layout`] is the one place a rectangle is
//! decided, and both the renderer and the pointer read its answer; and
//! modality is checked first in `handle` *and* in `handle_mouse`.
//!
//! Nothing here reaches into `discord::state` — it talks to `Handle` and to the
//! read-side queries on `State` — with one deliberate exception, [`fake`],
//! which is a core rather than a consumer of one.

pub mod app;
pub mod clipboard;
pub mod core_ext;
pub mod fake;
#[cfg(test)]
mod frames;
pub mod keymap;
pub mod layout;
pub mod login;
pub mod overlays;
pub mod panels;
pub mod status;
pub mod theme;
pub mod unread;
