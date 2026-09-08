//! Declarative policy engine

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Policy rule for capability access
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    pub capability: String,
    pub effect: PolicyEffect,
    pub conditions: HashMap<String, serde_json::Value>,
}

/// Policy effect: Allow or Deny
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PolicyEffect {
    Allow,
    Deny,
}

/// Complete policy document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub version: String,
    pub rules: Vec<PolicyRule>,
    pub default_effect: PolicyEffect,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            version: "1.0".to_string(),
            rules: Vec::new(),
            default_effect: PolicyEffect::Deny,
        }
    }
}

/// Policy engine for evaluating capabilities
pub struct PolicyEngine {
    policy: Policy,
}

impl PolicyEngine {
    pub fn new(policy: Policy) -> Self {
        Self { policy }
    }

    pub fn evaluate(
        &self,
        capability: &str,
        context: &HashMap<String, serde_json::Value>,
    ) -> PolicyEffect {
        // Find matching rules (first match wins)
        for rule in &self.policy.rules {
            if (rule.capability == capability || rule.capability == "*")
                && self.matches_conditions(&rule.conditions, context)
            {
                return rule.effect.clone();
            }
        }
        self.policy.default_effect.clone()
    }

    fn matches_conditions(
        &self,
        conditions: &HashMap<String, serde_json::Value>,
        context: &HashMap<String, serde_json::Value>,
    ) -> bool {
        for (key, expected) in conditions {
            match context.get(key) {
                Some(actual) if actual == expected => continue,
                Some(_) => return false,
                None => return false,
            }
        }
        true
    }
}
