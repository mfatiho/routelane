use routelane::domain::{HealthState, Profile};

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
