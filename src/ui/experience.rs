use crate::ui::i18n::Texts;
use routelane::domain::{HealthState, Profile};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusPresentation {
    pub title: &'static str,
    pub detail: &'static str,
    pub icon_name: &'static str,
}

pub fn status_presentation(
    health: HealthState,
    authorization: AuthorizationPresentation,
    texts: Texts,
) -> StatusPresentation {
    if matches!(authorization, AuthorizationPresentation::Denied)
        || matches!(health, HealthState::NeedsAuthorization)
    {
        return StatusPresentation {
            title: texts.authorization_denied,
            detail: texts.retry_authorization,
            icon_name: "dialog-password-symbolic",
        };
    }
    match health {
        HealthState::Inactive => StatusPresentation {
            title: texts.inactive,
            detail: texts.routing_inactive,
            icon_name: "media-playback-stop-symbolic",
        },
        HealthState::Applying => StatusPresentation {
            title: texts.status_applying,
            detail: texts.authorization_waiting,
            icon_name: "process-working-symbolic",
        },
        HealthState::Healthy => StatusPresentation {
            title: texts.status_healthy,
            detail: texts.routing_active,
            icon_name: "emblem-ok-symbolic",
        },
        HealthState::Degraded => StatusPresentation {
            title: texts.status_degraded,
            detail: texts.technical_details,
            icon_name: "dialog-warning-symbolic",
        },
        HealthState::Failed => StatusPresentation {
            title: texts.status_failed,
            detail: texts.technical_details,
            icon_name: "dialog-error-symbolic",
        },
        HealthState::NeedsAuthorization => unreachable!(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStep {
    AddLane,
    AddRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryAction {
    OpenPreferences,
    OpenRuleDialog,
    EnableRouting,
    DisableRouting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationPresentation {
    Idle,
    Waiting,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationAction {
    PresentExisting,
    CreateWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    Ignore,
    AskForConfirmation,
}

pub fn orphan_recovery_action(error_code: &str, retry_used: bool) -> RecoveryAction {
    if error_code == "DAEMON_ORPHANED_RESOURCES" && !retry_used {
        RecoveryAction::AskForConfirmation
    } else {
        RecoveryAction::Ignore
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashboardState {
    pub setup_step: Option<SetupStep>,
    pub primary_action: PrimaryAction,
    pub primary_action_enabled: bool,
    pub authorization: AuthorizationPresentation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorizationOutcome {
    pub authorization: AuthorizationPresentation,
    pub primary_action_enabled: bool,
    pub routing_active: bool,
}

pub fn activation_action(has_existing_window: bool) -> ActivationAction {
    if has_existing_window {
        ActivationAction::PresentExisting
    } else {
        ActivationAction::CreateWindow
    }
}

pub fn dashboard_state(
    profile: &Profile,
    health: HealthState,
    mutation_pending: bool,
) -> DashboardState {
    let setup_step = if profile.lanes.is_empty() {
        Some(SetupStep::AddLane)
    } else if profile.rules.is_empty() {
        Some(SetupStep::AddRule)
    } else {
        None
    };
    let primary_action = match setup_step {
        Some(SetupStep::AddLane) => PrimaryAction::OpenPreferences,
        Some(SetupStep::AddRule) => PrimaryAction::OpenRuleDialog,
        None if matches!(
            health,
            HealthState::Applying
                | HealthState::Healthy
                | HealthState::Degraded
                | HealthState::Failed
        ) =>
        {
            PrimaryAction::DisableRouting
        }
        None => PrimaryAction::EnableRouting,
    };

    DashboardState {
        setup_step,
        primary_action,
        primary_action_enabled: !mutation_pending,
        authorization: if mutation_pending {
            AuthorizationPresentation::Waiting
        } else {
            AuthorizationPresentation::Idle
        },
    }
}

pub fn authorization_after_error(
    routing_active: bool,
    authorization_denied: bool,
) -> AuthorizationOutcome {
    AuthorizationOutcome {
        authorization: if authorization_denied {
            AuthorizationPresentation::Denied
        } else {
            AuthorizationPresentation::Idle
        },
        primary_action_enabled: true,
        routing_active,
    }
}

pub fn authorization_after_applied() -> AuthorizationPresentation {
    AuthorizationPresentation::Idle
}
