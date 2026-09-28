//! Window bounds live under the app's config dir so parallel dev instances stay isolated.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::WebviewWindow;

// macOS screen coordinates are points; saving physical pixels can restore at
// the wrong scale before a newly created window is attached to its monitor.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct Bounds {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
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
            && self.right().is_finite()
            && self.bottom().is_finite()
    }

    fn right(self) -> f64 {
        self.x + self.width
    }

    fn bottom(self) -> f64 {
        self.y + self.height
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let width = self.right().min(other.right()) - x;
        let height = self.bottom().min(other.bottom()) - y;
        (width > 0.0 && height > 0.0).then_some(Self {
            x,
            y,
            width,
            height,
        })
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
    let screens = screens
        .iter()
        .copied()
        .filter(|screen| screen.valid())
        .collect::<Vec<_>>();
    if fully_covered(saved, &screens) {
        return Some(saved);
    }

    let titlebar = Bounds {
        height: saved.height.min(40.0),
        ..saved
    };
    let screen = screens.into_iter().max_by(|left, right| {
        let overlap = |screen: Bounds| {
            titlebar
                .intersection(screen)
                .map_or(0.0, |area| area.width * area.height)
        };
        overlap(*left).total_cmp(&overlap(*right))
    })?;
    titlebar.intersection(screen)?;

    let width = saved.width.min(screen.width);
    let height = saved.height.min(screen.height);
    Some(Bounds {
        x: saved.x.clamp(screen.x, screen.right() - width),
        y: saved.y.clamp(screen.y, screen.bottom() - height),
        width,
        height,
    })
}

fn fully_covered(saved: Bounds, screens: &[Bounds]) -> bool {
    let mut x_edges = vec![saved.x, saved.right()];
    for screen in screens {
        if let Some(overlap) = saved.intersection(*screen) {
            x_edges.extend([overlap.x, overlap.right()]);
        }
    }
    x_edges.sort_by(f64::total_cmp);
    x_edges.dedup();

    for edges in x_edges.windows(2) {
        let x_midpoint = edges[0].midpoint(edges[1]);
        let mut y_ranges = screens
            .iter()
            .filter(|screen| screen.x <= x_midpoint && x_midpoint < screen.right())
            .filter_map(|screen| saved.intersection(*screen))
            .map(|overlap| (overlap.y, overlap.bottom()))
            .collect::<Vec<_>>();
        y_ranges.sort_by(|left, right| left.0.total_cmp(&right.0));

        let mut covered_to = saved.y;
        for (start, end) in y_ranges {
            if start > covered_to {
                return false;
            }
            covered_to = covered_to.max(end);
        }
        if covered_to < saved.bottom() {
            return false;
        }
    }
    true
}

fn monitor_bounds(monitor: &tauri::Monitor) -> Option<Bounds> {
    let scale = monitor.scale_factor();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let position = monitor.work_area().position.to_logical::<f64>(scale);
    let size = monitor.work_area().size.to_logical::<f64>(scale);
    Some(Bounds {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    })
}

pub(crate) fn save_window_bounds(window: &WebviewWindow, path: &Path) -> Result<(), String> {
    // A fullscreen frame does not describe the normal window; keep the last saved frame.
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

pub(crate) fn startup_bounds(
    app: &tauri::AppHandle,
    path: &Path,
) -> Result<Option<Bounds>, String> {
    let Some(saved) = load_bounds(path) else {
        return Ok(None);
    };
    let screens = app
        .available_monitors()
        .map_err(|error| error.to_string())?
        .iter()
        .filter_map(monitor_bounds)
        .collect::<Vec<_>>();
    let Some(bounds) = restorable_bounds(saved, &screens) else {
        tracing::warn!(path = %path.display(), "saved window is off-screen; using default placement");
        return Ok(None);
    };
    Ok(Some(bounds))
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
    fn partially_offscreen_window_fits_on_the_remaining_display() {
        let saved = Bounds {
            x: 1280.0,
            y: 100.0,
            width: 1100.0,
            height: 720.0,
        };
        let screens = [Bounds {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        }];

        assert_eq!(
            restorable_bounds(saved, &screens),
            Some(Bounds { x: 340.0, ..saved })
        );
    }

    #[test]
    fn window_larger_than_the_display_shrinks_to_fit() {
        let saved = Bounds {
            x: 100.0,
            y: 100.0,
            width: 1600.0,
            height: 1000.0,
        };
        let screens = [Bounds {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        }];

        assert_eq!(restorable_bounds(saved, &screens), Some(screens[0]));
    }

    #[test]
    fn window_spanning_connected_displays_keeps_its_exact_frame() {
        let saved = Bounds {
            x: 700.0,
            y: 100.0,
            width: 1100.0,
            height: 700.0,
        };
        let screens = [
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 1000.0,
                height: 900.0,
            },
            Bounds {
                x: 1000.0,
                y: 0.0,
                width: 1000.0,
                height: 900.0,
            },
        ];

        assert_eq!(restorable_bounds(saved, &screens), Some(saved));
    }

    #[test]
    fn mirrored_displays_do_not_double_count_visible_space() {
        let saved = Bounds {
            x: 0.0,
            y: 100.0,
            width: 1000.0,
            height: 700.0,
        };
        let mirror = Bounds {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 900.0,
        };

        assert_eq!(
            restorable_bounds(saved, &[mirror, mirror]),
            Some(Bounds {
                width: 500.0,
                ..saved
            })
        );
    }

    #[test]
    fn secondary_display_to_the_left_keeps_negative_coordinates() {
        let saved = Bounds {
            x: -900.0,
            y: 100.0,
            width: 800.0,
            height: 700.0,
        };
        let screens = [
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 1200.0,
                height: 900.0,
            },
            Bounds {
                x: -1000.0,
                y: 0.0,
                width: 1000.0,
                height: 900.0,
            },
        ];

        assert_eq!(restorable_bounds(saved, &screens), Some(saved));
    }

    #[test]
    fn corrupt_bounds_do_not_block_launch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.yaml");
        std::fs::write(&path, "not: [valid yaml").unwrap();

        assert_eq!(load_bounds(&path), None);
    }
}
