//! Clap value-enums for `fj pr` flags and their conversions into the API
//! layer's enums (kept out of pr.rs to slim the command file).

use clap::ValueEnum;

use crate::api::issue::State;
use crate::api::pull::{MergeStyle, ReviewEvent};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StateFilter {
    Open,
    Closed,
    All,
}

impl From<StateFilter> for State {
    fn from(value: StateFilter) -> Self {
        match value {
            StateFilter::Open => State::Open,
            StateFilter::Closed => State::Closed,
            StateFilter::All => State::All,
        }
    }
}

impl StateFilter {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StateFilter::Open => "open",
            StateFilter::Closed => "closed",
            StateFilter::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MergeStyleArg {
    Merge,
    Rebase,
    RebaseMerge,
    Squash,
}

impl From<MergeStyleArg> for MergeStyle {
    fn from(value: MergeStyleArg) -> Self {
        match value {
            MergeStyleArg::Merge => MergeStyle::Merge,
            MergeStyleArg::Rebase => MergeStyle::Rebase,
            MergeStyleArg::RebaseMerge => MergeStyle::RebaseMerge,
            MergeStyleArg::Squash => MergeStyle::Squash,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReviewEventArg {
    Approve,
    RequestChanges,
    Comment,
}

impl From<ReviewEventArg> for ReviewEvent {
    fn from(value: ReviewEventArg) -> Self {
        match value {
            ReviewEventArg::Approve => ReviewEvent::Approve,
            ReviewEventArg::RequestChanges => ReviewEvent::RequestChanges,
            ReviewEventArg::Comment => ReviewEvent::Comment,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum UpdateStyle {
    Merge,
    Rebase,
}

impl UpdateStyle {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            UpdateStyle::Merge => "merge",
            UpdateStyle::Rebase => "rebase",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_filter_maps_to_api_state() {
        assert!(matches!(State::from(StateFilter::Open), State::Open));
        assert!(matches!(State::from(StateFilter::Closed), State::Closed));
        assert!(matches!(State::from(StateFilter::All), State::All));
        assert_eq!(StateFilter::Open.as_str(), "open");
        assert_eq!(StateFilter::Closed.as_str(), "closed");
        assert_eq!(StateFilter::All.as_str(), "all");
    }

    #[test]
    fn merge_style_maps_to_api_style() {
        assert!(matches!(
            MergeStyle::from(MergeStyleArg::Merge),
            MergeStyle::Merge
        ));
        assert!(matches!(
            MergeStyle::from(MergeStyleArg::Rebase),
            MergeStyle::Rebase
        ));
        assert!(matches!(
            MergeStyle::from(MergeStyleArg::RebaseMerge),
            MergeStyle::RebaseMerge
        ));
        assert!(matches!(
            MergeStyle::from(MergeStyleArg::Squash),
            MergeStyle::Squash
        ));
    }

    #[test]
    fn review_event_maps_to_api_event() {
        assert!(matches!(
            ReviewEvent::from(ReviewEventArg::Approve),
            ReviewEvent::Approve
        ));
        assert!(matches!(
            ReviewEvent::from(ReviewEventArg::RequestChanges),
            ReviewEvent::RequestChanges
        ));
        assert!(matches!(
            ReviewEvent::from(ReviewEventArg::Comment),
            ReviewEvent::Comment
        ));
    }

    #[test]
    fn update_style_as_str() {
        assert_eq!(UpdateStyle::Merge.as_str(), "merge");
        assert_eq!(UpdateStyle::Rebase.as_str(), "rebase");
    }
}
