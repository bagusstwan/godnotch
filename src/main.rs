#![cfg_attr(not(test), windows_subsystem = "windows")]

mod app;
mod core;
mod modules;
mod platform;

use windows::core::w;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(debug_assertions)]
    let t_main = std::time::Instant::now();

    // 1. Single instance check using named mutex
    // If another instance is already running, exit cleanly and silently.
    let mutex_name = w!("Local\\godnotch_single_instance");
    let _instance_guard = match platform::SingleInstance::try_acquire(mutex_name) {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            // Already running; terminate second instance cleanly.
            return Ok(());
        }
        Err(err) => {
            return Err(Box::new(err));
        }
    };

    // 2. Set process DPI awareness to Per-Monitor V2
    let _ = platform::set_dpi_awareness_v2();

    // 3. Initialize central application state
    let _app = app::App::new();

    // 4. Create and display interactive layered notch window (T1.1..T1.3)
    let _notch_window = platform::NotchWindow::create()?;

    #[cfg(debug_assertions)]
    println!(
        "[STARTUP_PROFILE] Total main() startup to window presented: {:.3} ms",
        t_main.elapsed().as_secs_f64() * 1000.0
    );

    // 5. Run blocking Win32 message loop (GetMessageW)
    platform::run_message_loop()?;

    Ok(())
}
