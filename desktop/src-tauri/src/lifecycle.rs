use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitPhase {
    Running,
    Checking,
    Confirming,
    ShuttingDown,
    Approved,
}

/// Coordinates application Quit without changing work until the user decides.
/// Window close is a separate hide/show policy.
pub struct QuitCoordinator {
    phase: Mutex<QuitPhase>,
}

impl QuitCoordinator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: Mutex::new(QuitPhase::Running),
        }
    }

    pub fn begin(&self) -> bool {
        let mut phase = lock(&self.phase);
        if *phase != QuitPhase::Running {
            return false;
        }
        *phase = QuitPhase::Checking;
        true
    }

    pub fn begin_confirmation(&self) -> bool {
        let mut phase = lock(&self.phase);
        if *phase != QuitPhase::Checking {
            return false;
        }
        *phase = QuitPhase::Confirming;
        true
    }

    pub fn begin_shutdown(&self) -> bool {
        let mut phase = lock(&self.phase);
        if !matches!(*phase, QuitPhase::Checking | QuitPhase::Confirming) {
            return false;
        }
        *phase = QuitPhase::ShuttingDown;
        true
    }

    pub fn cancel(&self) {
        let mut phase = lock(&self.phase);
        if matches!(*phase, QuitPhase::Checking | QuitPhase::Confirming) {
            *phase = QuitPhase::Running;
        }
    }

    pub fn approve_exit(&self) {
        let mut phase = lock(&self.phase);
        if *phase == QuitPhase::ShuttingDown {
            *phase = QuitPhase::Approved;
        }
    }

    #[must_use]
    pub fn exit_is_approved(&self) -> bool {
        *lock(&self.phase) == QuitPhase::Approved
    }

    #[cfg(test)]
    fn phase(&self) -> QuitPhase {
        *lock(&self.phase)
    }
}

impl Default for QuitCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_quit_restores_running_state() {
        let coordinator = QuitCoordinator::new();

        assert!(coordinator.begin());
        assert!(coordinator.begin_confirmation());
        coordinator.cancel();

        assert_eq!(coordinator.phase(), QuitPhase::Running);
        assert!(coordinator.begin());
    }

    #[test]
    fn repeated_quit_requests_are_single_flight_and_approved_exit_passes() {
        let coordinator = QuitCoordinator::new();

        assert!(coordinator.begin());
        assert!(!coordinator.begin());
        assert!(coordinator.begin_shutdown());
        coordinator.approve_exit();

        assert!(coordinator.exit_is_approved());
        assert!(!coordinator.begin());
    }
}
