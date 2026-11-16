#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod tile;
pub mod win;

use tracing::{Level, info};

fn main() -> Result<(), win::WinError> {
    tracing_subscriber::fmt().with_max_level(Level::DEBUG).init();
    win::init_dpi();

    let snapped = win::apply_foreground(tile::Action::Left, 8)?;
    info!(snapped, "Applied Left tile action");

    Ok(())
}
