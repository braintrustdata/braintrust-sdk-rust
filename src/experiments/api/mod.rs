//! Internal API contracts and traits.
//!
//! This module contains internal API types and traits used for communicating
//! with the Braintrust backend. These are not part of the public API.

pub(crate) mod comparison;
pub(crate) mod registration;
pub(crate) mod traits;

pub(crate) use comparison::ExperimentComparisonResponse;
pub(crate) use registration::{
    BaseExperimentRequest, BaseExperimentResponse, ExperimentRegisterRequest,
    ExperimentRegisterResponse,
};
pub(crate) use traits::{BaseExperimentFetcher, ExperimentComparisonFetcher, ExperimentRegistrar};
