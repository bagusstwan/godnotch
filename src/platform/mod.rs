//! Platform abstraction layer for Windows APIs.
//!
//! According to SAD.md section 3 and AGENTS.md, this module is the SOLE location
//! in the codebase permitted to contain `unsafe` blocks and direct Win32/WinRT calls.
//! All higher layers (main, app, core, modules) interact with Windows exclusively
//! through safe abstractions provided here.

pub mod audio;
pub mod autostart;
pub mod bluetooth;
pub mod dragdrop;
pub mod fullscreen;
pub mod http;
pub mod media;
pub mod power;
pub mod single_instance;
pub mod tray;
pub mod window;

pub use single_instance::SingleInstance;
pub use window::{NotchWindow, run_message_loop, set_dpi_awareness_v2};
