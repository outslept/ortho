pub mod tile;
pub mod win;

fn main() {
    win::init_dpi();

    let mut history = win::RestoreHistory::new();

    if let Err(err) = win::apply_foreground(tile::Action::Left, 8, &mut history) {
        eprintln!("orthowm: failed to tile foreground window: {err}");
    }

    let _ = (win::restore_foreground, win::move_foreground_monitor);
}
