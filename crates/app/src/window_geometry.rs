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

#[derive(Clone, Copy)]
struct DisplayBounds {
    full: Bounds,
    work_area: Bounds,
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

fn restorable_bounds(saved: Bounds, displays: &[DisplayBounds]) -> Option<Bounds> {
    if !saved.valid() {
        return None;
    }
    let displays = displays
        .iter()
        .copied()
        .filter(|display| display.full.valid() && display.work_area.valid())
        .collect::<Vec<_>>();
    let titlebar = Bounds {
        height: saved.height.min(40.0),
        ..saved
    };

    let full_bounds = displays
        .iter()
        .map(|display| display.full)
        .collect::<Vec<_>>();
    let work_areas = displays
        .iter()
        .map(|display| display.work_area)
        .collect::<Vec<_>>();
    if fully_covered(saved, &full_bounds) && titlebar_reachable(titlebar, &work_areas) {
        return Some(saved);
    }

    let work_area = displays
        .into_iter()
        .max_by(|left, right| {
            let overlap = |display: DisplayBounds| {
                titlebar
                    .intersection(display.work_area)
                    .map_or(0.0, |area| area.width * area.height)
            };
            overlap(*left).total_cmp(&overlap(*right))
        })?
        .work_area;
    titlebar.intersection(work_area)?;

    let width = saved.width.min(work_area.width);
    let height = saved.height.min(work_area.height);
    Some(Bounds {
        x: saved.x.min(work_area.right() - width).max(work_area.x),
        y: saved.y.min(work_area.bottom() - height).max(work_area.y),
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

fn titlebar_reachable(titlebar: Bounds, work_areas: &[Bounds]) -> bool {
    let mut spans = work_areas
        .iter()
        .filter(|work_area| work_area.y <= titlebar.y && work_area.bottom() >= titlebar.bottom())
        .filter_map(|work_area| titlebar.intersection(*work_area))
        .map(|overlap| (overlap.x, overlap.right()))
        .collect::<Vec<_>>();
    spans.sort_by(|left, right| left.0.total_cmp(&right.0));

    let mut span_start = 0.0;
    let mut span_end = f64::NEG_INFINITY;
    for (left, right) in spans {
        if left > span_end {
            span_start = left;
            span_end = right;
        } else {
            span_end = span_end.max(right);
        }
        // A narrow sliver can expose only controls, leaving no place to drag.
        if span_end - span_start >= titlebar.width.min(160.0) {
            return true;
        }
    }
    false
}

fn monitor_bounds(monitor: &tauri::Monitor) -> Option<DisplayBounds> {
    let scale = monitor.scale_factor();
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let bounds = |position: tauri::PhysicalPosition<i32>, size: tauri::PhysicalSize<u32>| {
        let position = position.to_logical::<f64>(scale);
        let size = size.to_logical::<f64>(scale);
        Bounds {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        }
    };
    Some(DisplayBounds {
        full: bounds(*monitor.position(), *monitor.size()),
        work_area: bounds(monitor.work_area().position, monitor.work_area().size),
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
    use super::{Bounds, DisplayBounds, load_bounds, restorable_bounds, save_bounds};

    fn displays(screens: &[Bounds]) -> Vec<DisplayBounds> {
        screens
            .iter()
            .map(|screen| DisplayBounds {
                full: *screen,
                work_area: *screen,
            })
            .collect()
    }

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
        assert_eq!(restorable_bounds(bounds, &displays(&screens)), Some(bounds));
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

        assert_eq!(restorable_bounds(saved, &displays(&remaining_screen)), None);
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
            restorable_bounds(saved, &displays(&screens)),
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

        assert_eq!(
            restorable_bounds(saved, &displays(&screens)),
            Some(screens[0])
        );
    }

    #[test]
    fn fractional_display_coordinates_do_not_prevent_launch() {
        let saved = Bounds {
            x: 0.3,
            y: 100.0,
            width: 1600.0,
            height: 700.0,
        };
        let screen = Bounds {
            x: 0.3,
            y: 0.0,
            width: 1200.6,
            height: 900.0,
        };

        assert_eq!(
            restorable_bounds(saved, &displays(&[screen])),
            Some(Bounds {
                width: screen.width,
                ..saved
            })
        );
    }

    #[test]
    fn window_partly_behind_the_dock_keeps_its_saved_frame() {
        let saved = Bounds {
            x: 100.0,
            y: 100.0,
            width: 800.0,
            height: 700.0,
        };
        let work_area = Bounds {
            x: 0.0,
            y: 25.0,
            width: 1000.0,
            height: 700.0,
        };

        let full = Bounds {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 900.0,
        };
        assert_eq!(
            restorable_bounds(saved, &[DisplayBounds { full, work_area }]),
            Some(saved)
        );
    }

    #[test]
    fn window_partly_behind_a_side_dock_keeps_its_saved_frame() {
        let saved = Bounds {
            x: 0.0,
            y: 100.0,
            width: 800.0,
            height: 700.0,
        };
        let full = Bounds {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 900.0,
        };
        let work_area = Bounds {
            x: 80.0,
            width: 920.0,
            ..full
        };

        assert_eq!(
            restorable_bounds(saved, &[DisplayBounds { full, work_area }]),
            Some(saved)
        );
    }

    #[test]
    fn inaccessible_titlebar_is_moved_into_the_work_area() {
        let saved = Bounds {
            x: 100.0,
            y: 10.0,
            width: 800.0,
            height: 700.0,
        };
        let full = Bounds {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 900.0,
        };
        let work_area = Bounds {
            y: 40.0,
            height: 860.0,
            ..full
        };

        assert_eq!(
            restorable_bounds(saved, &[DisplayBounds { full, work_area }]),
            Some(Bounds { y: 40.0, ..saved })
        );
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

        assert_eq!(restorable_bounds(saved, &displays(&screens)), Some(saved));
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
            restorable_bounds(saved, &displays(&[mirror, mirror])),
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

        assert_eq!(restorable_bounds(saved, &displays(&screens)), Some(saved));
    }

    #[test]
    fn corrupt_bounds_do_not_block_launch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.yaml");
        std::fs::write(&path, "not: [valid yaml").unwrap();

        assert_eq!(load_bounds(&path), None);
    }
}
