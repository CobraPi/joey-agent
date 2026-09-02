//! Risk assessment model (spec 023, FR-022).
//!
//! A [`RiskAssessment`] summarizes the risk of a proposed change from a set
//! of detected [`RiskFactor`]s. The `assess` function implements the FR-022
//! High/Medium/Low rule used to gate routing and verification decisions.

use serde::{Deserialize, Serialize};

/// Overall risk level for a change (FR-022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl Default for RiskLevel {
    fn default() -> Self {
        RiskLevel::Low
    }
}

/// The kind of risk factor detected in a change (FR-022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskFactorKind {
    PublicApiExposure,
    SecuritySensitive,
    Concurrency,
    FanOut,
    OwnershipBoundary,
}

/// A single detected risk factor with supporting evidence (FR-022).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RiskFactor {
    pub kind: RiskFactorKind,
    pub evidence: String,
    pub weight: u8,
}

/// The result of assessing a set of risk factors (FR-022).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RiskAssessment {
    pub level: RiskLevel,
    pub factors: Vec<RiskFactor>,
}

/// FanOut factor weight that must be EXCEEDED for High risk (FR-022:
/// weight > this threshold ⇒ High; weight == this ⇒ at most Medium).
pub const FANOUT_HIGH_WEIGHT: u8 = 2;

/// OwnershipBoundary factor weight that must be EXCEEDED for High risk
/// (FR-022: weight > this threshold ⇒ High; weight == this ⇒ at most
/// Medium).
pub const OWNERSHIP_HIGH_WEIGHT: u8 = 2;

/// Assess the risk level of a change from its detected factors (FR-022).
///
/// Rule, exactly:
/// - **High** iff any factor kind is `PublicApiExposure`,
///   `SecuritySensitive`, or `Concurrency` (any weight ≥ 1), OR any `FanOut`
///   factor weight > [`FANOUT_HIGH_WEIGHT`], OR any `OwnershipBoundary`
///   factor weight > [`OWNERSHIP_HIGH_WEIGHT`].
/// - Else **Medium** iff any factor weight >= 2 OR there are >= 3 factors.
/// - Else **Low**.
///
/// The returned assessment carries the factors it was assessed from.
pub fn assess(factors: Vec<RiskFactor>) -> RiskAssessment {
    let level = if factors.iter().any(|f| {
        matches!(
            f.kind,
            RiskFactorKind::PublicApiExposure
                | RiskFactorKind::SecuritySensitive
                | RiskFactorKind::Concurrency
        ) || (f.kind == RiskFactorKind::FanOut && f.weight > FANOUT_HIGH_WEIGHT)
            || (f.kind == RiskFactorKind::OwnershipBoundary
                && f.weight > OWNERSHIP_HIGH_WEIGHT)
    }) {
        RiskLevel::High
    } else if factors.iter().any(|f| f.weight >= 2) || factors.len() >= 3 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };

    RiskAssessment { level, factors }
}

impl RiskAssessment {
    /// Whether this assessment is High risk (FR-022).
    pub fn is_high(&self) -> bool {
        self.level == RiskLevel::High
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factor(kind: RiskFactorKind, weight: u8) -> RiskFactor {
        RiskFactor {
            kind,
            evidence: format!("{kind:?} evidence"),
            weight,
        }
    }

    #[test]
    fn empty_factors_is_low() {
        let a = assess(vec![]);
        assert_eq!(a.level, RiskLevel::Low);
        assert!(!a.is_high());
        assert!(a.factors.is_empty());
    }

    #[test]
    fn public_api_exposure_weight_1_is_high() {
        let a = assess(vec![factor(RiskFactorKind::PublicApiExposure, 1)]);
        assert_eq!(a.level, RiskLevel::High);
        assert!(a.is_high());
    }

    #[test]
    fn security_sensitive_is_high() {
        let a = assess(vec![factor(RiskFactorKind::SecuritySensitive, 1)]);
        assert_eq!(a.level, RiskLevel::High);
    }

    #[test]
    fn concurrency_is_high() {
        let a = assess(vec![factor(RiskFactorKind::Concurrency, 1)]);
        assert_eq!(a.level, RiskLevel::High);
    }

    #[test]
    fn fan_out_weight_2_is_medium_not_high() {
        let a = assess(vec![factor(RiskFactorKind::FanOut, 2)]);
        assert_eq!(a.level, RiskLevel::Medium);
        assert!(!a.is_high());
    }

    #[test]
    fn fan_out_weight_3_is_high() {
        let a = assess(vec![factor(RiskFactorKind::FanOut, 3)]);
        assert_eq!(a.level, RiskLevel::High);
    }

    #[test]
    fn ownership_boundary_weight_2_is_medium_3_is_high() {
        let medium = assess(vec![factor(RiskFactorKind::OwnershipBoundary, 2)]);
        assert_eq!(medium.level, RiskLevel::Medium);
        let high = assess(vec![factor(RiskFactorKind::OwnershipBoundary, 3)]);
        assert_eq!(high.level, RiskLevel::High);
    }

    #[test]
    fn fan_out_weight_1_alone_is_low() {
        // weight 1 < 2 (Medium bar) and factors.len() 1 < 3 ⇒ Low.
        let a = assess(vec![factor(RiskFactorKind::FanOut, 1)]);
        assert_eq!(a.level, RiskLevel::Low);
    }

    #[test]
    fn three_low_weight_factors_is_medium() {
        let a = assess(vec![
            factor(RiskFactorKind::FanOut, 1),
            factor(RiskFactorKind::OwnershipBoundary, 1),
            factor(RiskFactorKind::FanOut, 1),
        ]);
        assert_eq!(a.level, RiskLevel::Medium);
    }

    #[test]
    fn assessment_carries_factors() {
        let factors = vec![
            factor(RiskFactorKind::Concurrency, 1),
            factor(RiskFactorKind::FanOut, 1),
        ];
        let a = assess(factors.clone());
        assert_eq!(a.factors, factors);
    }

    #[test]
    fn serde_risk_level_high_is_lowercase() {
        assert_eq!(serde_json::to_string(&RiskLevel::High).unwrap(), "\"high\"");
        assert_eq!(
            serde_json::from_str::<RiskLevel>("\"high\"").unwrap(),
            RiskLevel::High
        );
    }

    #[test]
    fn serde_risk_factor_kind_snake_case() {
        assert_eq!(
            serde_json::to_string(&RiskFactorKind::PublicApiExposure).unwrap(),
            "\"public_api_exposure\""
        );
        assert_eq!(
            serde_json::from_str::<RiskFactorKind>("\"public_api_exposure\"").unwrap(),
            RiskFactorKind::PublicApiExposure
        );
        assert_eq!(
            serde_json::to_string(&RiskFactorKind::OwnershipBoundary).unwrap(),
            "\"ownership_boundary\""
        );
    }

    #[test]
    fn serde_field_names() {
        let a = RiskAssessment {
            level: RiskLevel::Low,
            factors: vec![RiskFactor {
                kind: RiskFactorKind::FanOut,
                evidence: "e".to_string(),
                weight: 1,
            }],
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"level\""));
        assert!(json.contains("\"factors\""));
        assert!(json.contains("\"kind\""));
        assert!(json.contains("\"evidence\""));
        assert!(json.contains("\"weight\""));
        // Round-trip.
        let back: RiskAssessment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn default_is_low_and_empty() {
        let d = RiskAssessment::default();
        assert_eq!(d.level, RiskLevel::Low);
        assert!(d.factors.is_empty());
        assert_eq!(RiskLevel::default(), RiskLevel::Low);
    }
}
