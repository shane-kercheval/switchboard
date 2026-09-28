use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::{LogicalPosition, LogicalSize, WebviewWindow};

// macOS screen coordinates are points; saving physical pixels can restore at
// the wrong scale before a newly created window is attached to its monitor.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
struct Bounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl Bounds {
    fn valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
            && self.width <= 16_384.0
            && self.height <= 16_384.0
    }
}

fn save_bounds(path: &Path, bounds: Bounds) -> Result<(), String> {
    if !bounds.valid() {
        return Err("window bounds are invalid".to_owned());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    switchboard_core::write_yaml(path, &bounds).map_err(|error| error.to_string())
}

fn load_bounds(path: &Path) -> Option<Bounds> {
    if !path.exists() {
        return None;
    }
    match switchboard_core::read_yaml::<Bounds>(path) {
        Ok(bounds) if bounds.valid() => Some(bounds),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "saved window bounds are invalid; using default placement");
            None
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "saved window bounds could not be read; using default placement");
            None
        }
    }
}

fn restorable_bounds(saved: Bounds, screens: &[Bounds]) -> Option<Bounds> {
    if !saved.valid() {
        return None;
    }
    screens
        .iter()
        .filter(|screen| screen.valid())
        .any(|screen| {
            // Keep the titlebar reachable; an off-screen window cannot be dragged back.
            let overlap_left = saved.x.max(screen.x);
            let overlap_right = (saved.x + saved.width).min(screen.x + screen.width);
            let titlebar_visible =
                saved.y >= screen.y && saved.y + 40.0 <= screen.y + screen.height;
            overlap_right - overlap_left >= saved.width.min(160.0) && titlebar_visible
        })
        .then_some(saved)
}

fn monitor_bounds(monitor: &tauri::Monitor) -> Option<Bounds> {
    let scale = monitor.scale_factor();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let position = monitor.position().to_logical::<f64>(scale);
    let size = monitor.size().to_logical::<f64>(scale);
    Some(Bounds {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    })
}

pub(crate) fn save_window_bounds(window: &WebviewWindow, path: &Path) -> Result<(), String> {
    // Fullscreen uses a separate Space and does not describe the normal window frame.
    if window.is_fullscreen().map_err(|error| error.to_string())? {
        return Ok(());
    }
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    if !scale.is_finite() || scale <= 0.0 {
        return Err("window scale factor is invalid".to_owned());
    }
    let position = window
        .outer_position()
        .map_err(|error| error.to_string())?
        .to_logical::<f64>(scale);
    let size = window
        .inner_size()
        .map_err(|error| error.to_string())?
        .to_logical::<f64>(scale);
    save_bounds(
        path,
        Bounds {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        },
    )
}

pub(crate) fn restore_window_bounds(window: &WebviewWindow, path: &Path) -> Result<(), String> {
    let Some(saved) = load_bounds(path) else {
        return Ok(());
    };
    let screens = window
        .available_monitors()
        .map_err(|error| error.to_string())?
        .iter()
        .filter_map(monitor_bounds)
        .collect::<Vec<_>>();
    let Some(bounds) = restorable_bounds(saved, &screens) else {
        tracing::warn!(path = %path.display(), "saved window is off-screen; using default placement");
        return Ok(());
    };
    window
        .set_size(LogicalSize::new(bounds.width, bounds.height))
        .map_err(|error| error.to_string())?;
    window
        .set_position(LogicalPosition::new(bounds.x, bounds.y))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{Bounds, load_bounds, restorable_bounds, save_bounds};

    #[test]
    fn saved_bounds_round_trip_and_restore_on_the_same_screen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.yaml");
        let bounds = Bounds {
            x: 210.0,
            y: 135.0,
            width: 1100.0,
            height: 720.0,
        };
        save_bounds(&path, bounds).unwrap();
        let screens = [Bounds {
            x: 0.0,
            y: 0.0,
            width: 1728.0,
            height: 1117.0,
        }];

        assert_eq!(load_bounds(&path), Some(bounds));
        assert_eq!(restorable_bounds(bounds, &screens), Some(bounds));
    }

    #[test]
    fn disconnected_screen_falls_back_to_default_placement() {
        let saved = Bounds {
            x: 2200.0,
            y: 100.0,
            width: 1100.0,
            height: 720.0,
        };
        let remaining_screen = [Bounds {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        }];

        assert_eq!(restorable_bounds(saved, &remaining_screen), None);
    }

    #[test]
    fn corrupt_bounds_do_not_block_launch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.yaml");
        std::fs::write(&path, "not: [valid yaml").unwrap();

        assert_eq!(load_bounds(&path), None);
    }
}
